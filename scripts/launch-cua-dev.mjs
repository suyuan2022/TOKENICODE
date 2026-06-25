#!/usr/bin/env node

// Launch TOKENICODE's alpha debug build through a unique .app wrapper so
// Computer Use can target the development client instead of an installed app.

import { execFileSync, spawn } from 'node:child_process';
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import http from 'node:http';
import net from 'node:net';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const scriptsDir = dirname(__filename);
const projectRoot = resolve(scriptsDir, '..');
const tauriDir = join(projectRoot, 'src-tauri');
const debugDir = join(tauriDir, 'target', 'debug');

const devServerHost = '127.0.0.1';
const devServerPort = 1422;
const bundleId = 'com.tinyzhuang.tcalpha.cuadev';
const appName = 'TCAlpha CUA Dev';
const sourceBin = join(debugDir, 'tokenicode');
const wrapperApp = join(debugDir, `${appName}.app`);
const currentBin = join(wrapperApp, 'Contents', 'MacOS', 'tokenicode-cua-dev');
const launcherName = 'tokenicode-cua-dev-launcher';
const launcherPath = join(wrapperApp, 'Contents', 'MacOS', launcherName);
const launcherSourcePath = join(debugDir, `${launcherName}.c`);
const logPath = '/tmp/tokenicode-cua-dev.log';
const launchServicesRegister = '/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister';

const tauriConfig = {
  productName: appName,
  identifier: bundleId,
  build: {
    devUrl: `http://${devServerHost}:${devServerPort}`,
  },
  app: {
    windows: [
      {
        title: appName,
        width: 1280,
        height: 800,
        minWidth: 900,
        minHeight: 600,
        titleBarStyle: 'Overlay',
        hiddenTitle: true,
      },
    ],
  },
  bundle: {
    icon: [
      '../editions/alpha/icons/32x32.png',
      '../editions/alpha/icons/128x128.png',
      '../editions/alpha/icons/128x128@2x.png',
      '../editions/alpha/icons/icon.icns',
      '../editions/alpha/icons/icon.ico',
    ],
  },
};

function parseArgs(argv) {
  const flags = new Set(argv.filter((arg) => arg.startsWith('--')));
  if (flags.has('--help') || flags.has('-h')) {
    process.stdout.write(`Usage: pnpm run dev:cua -- [--no-build] [--no-open]\n\nLaunches ${appName} for Computer Use testing.\n`);
    process.exit(0);
  }
  return {
    build: !flags.has('--no-build'),
    open: !flags.has('--no-open'),
  };
}

function log(message) {
  process.stderr.write(`[launch-cua-dev] ${message}\n`);
}

function hostTriple() {
  const output = capture('rustc', ['-vV']);
  const match = output.match(/^host:\s*(.+)$/m);
  return match?.[1]?.trim() || 'aarch64-apple-darwin';
}

function cuaEnv() {
  return {
    ...process.env,
    EDITION: 'alpha',
    TOKENICODE_CUA_DEV: '1',
    TAURI_CONFIG: JSON.stringify(tauriConfig),
    TAURI_ENV_TARGET_TRIPLE: hostTriple(),
    TAURI_ANDROID_PACKAGE_NAME_PREFIX: 'com_tinyzhuang',
    TAURI_ANDROID_PACKAGE_NAME_APP_NAME: 'tcalpha_cuadev',
  };
}

function run(cmd, args, opts = {}) {
  log(`${cmd} ${args.join(' ')}`);
  execFileSync(cmd, args, {
    cwd: opts.cwd || projectRoot,
    stdio: opts.stdio || 'inherit',
    env: opts.env || cuaEnv(),
  });
}

function capture(cmd, args, opts = {}) {
  try {
    return execFileSync(cmd, args, {
      cwd: opts.cwd || projectRoot,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
      env: opts.env || { ...process.env },
    });
  } catch {
    return '';
  }
}

function readText(filePath) {
  try {
    return readFileSync(filePath, 'utf8');
  } catch {
    return '';
  }
}

function sleep(ms) {
  return new Promise((resolveSleep) => setTimeout(resolveSleep, ms));
}

async function waitFor(predicate, timeoutMs, intervalMs = 250) {
  const started = Date.now();
  while (Date.now() - started < timeoutMs) {
    const value = await predicate();
    if (value) return value;
    await sleep(intervalMs);
  }
  return null;
}

function listProcesses() {
  return capture('ps', ['-axo', 'pid=,ppid=,command='])
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      const match = line.match(/^(\d+)\s+(\d+)\s+(.+)$/);
      return match
        ? { pid: Number(match[1]), ppid: Number(match[2]), command: match[3] }
        : null;
    })
    .filter(Boolean);
}

function commandStartsWith(command, executable) {
  return command === executable || command.startsWith(`${executable} `);
}

function processCwd(pid) {
  const output = capture('lsof', ['-a', '-p', String(pid), '-d', 'cwd', '-Fn']);
  return output
    .split('\n')
    .find((line) => line.startsWith('n'))
    ?.slice(1)
    .trim() || '';
}

function descendantsOf(rootPids, processes) {
  const byParent = new Map();
  for (const proc of processes) {
    const children = byParent.get(proc.ppid) || [];
    children.push(proc);
    byParent.set(proc.ppid, children);
  }

  const result = new Set(rootPids);
  const queue = [...rootPids];
  while (queue.length > 0) {
    const pid = queue.shift();
    for (const child of byParent.get(pid) || []) {
      if (!result.has(child.pid)) {
        result.add(child.pid);
        queue.push(child.pid);
      }
    }
  }
  return result;
}

function alivePids(pids) {
  const alive = new Set();
  for (const proc of listProcesses()) {
    if (pids.has(proc.pid)) alive.add(proc.pid);
  }
  return alive;
}

async function terminatePids(pids, label) {
  if (pids.length === 0) return { targeted: [], survivors: [] };

  const processes = listProcesses();
  const allPids = descendantsOf(pids, processes);
  log(`stopping ${label}: ${[...allPids].join(', ')}`);
  for (const pid of allPids) {
    try {
      process.kill(pid, 'SIGTERM');
    } catch {
      // Already gone.
    }
  }

  await waitFor(() => alivePids(allPids).size === 0, 2500, 250);
  for (const pid of alivePids(allPids)) {
    try {
      process.kill(pid, 'SIGKILL');
    } catch {
      // Already gone or unkillable.
    }
  }

  await waitFor(() => alivePids(allPids).size === 0, 2000, 250);
  return { targeted: [...allPids], survivors: [...alivePids(allPids)] };
}

async function stopOldCuaDevInstances() {
  capture('osascript', ['-e', `tell application id "${bundleId}" to quit`]);
  await sleep(500);

  const currentPids = listProcesses()
    .filter((proc) => {
      if (commandStartsWith(proc.command, currentBin)) return true;

      // Plain `pnpm tauri dev` starts `target/debug/tokenicode` without a
      // bundle id. Treat only this worktree's process as stale dev state.
      if (
        (proc.command === 'target/debug/tokenicode' || commandStartsWith(proc.command, sourceBin))
        && processCwd(proc.pid) === tauriDir
      ) {
        return true;
      }

      if (
        proc.command.includes(projectRoot)
        && (
          proc.command.includes('pnpm tauri dev')
          || proc.command.includes('@tauri-apps/cli/tauri.js dev')
        )
      ) {
        return true;
      }

      return false;
    })
    .map((proc) => proc.pid);
  return terminatePids(currentPids, 'old TOKENICODE development process tree');
}

function portOpen(port) {
  return new Promise((resolveOpen) => {
    const socket = net.createConnection({ host: devServerHost, port });
    socket.once('connect', () => {
      socket.destroy();
      resolveOpen(true);
    });
    socket.once('error', () => resolveOpen(false));
    socket.setTimeout(1000, () => {
      socket.destroy();
      resolveOpen(false);
    });
  });
}

function httpGet(pathname) {
  return new Promise((resolveGet) => {
    const req = http.get(
      { host: devServerHost, port: devServerPort, path: pathname, timeout: 2000 },
      (res) => {
        let body = '';
        res.setEncoding('utf8');
        res.on('data', (chunk) => { body += chunk; });
        res.on('end', () => resolveGet({ statusCode: res.statusCode || 0, body }));
      },
    );
    req.on('error', () => resolveGet(null));
    req.on('timeout', () => {
      req.destroy();
      resolveGet(null);
    });
  });
}

async function devServerReady() {
  if (!(await portOpen(devServerPort))) return false;
  const root = await httpGet('/');
  return !!root
    && root.statusCode === 200
    && root.body.includes('/@vite/client')
    && root.body.includes('/src/main.tsx');
}

function devServerListenerPids() {
  return [
    ...new Set(
      capture('lsof', ['-nP', `-tiTCP:${devServerPort}`, '-sTCP:LISTEN'])
        .split(/\s+/)
        .map((pid) => Number(pid))
        .filter((pid) => Number.isInteger(pid) && pid > 0),
    ),
  ];
}

function processCommand(pid) {
  return listProcesses().find((proc) => proc.pid === pid)?.command || '';
}

function isCurrentProjectProcess(pid) {
  const cwd = processCwd(pid);
  return cwd === projectRoot || cwd === tauriDir || processCommand(pid).includes(projectRoot);
}

async function stopCurrentWorktreeDevServer() {
  const targetPids = devServerListenerPids()
    .filter((pid) => isCurrentProjectProcess(pid));
  return terminatePids(targetPids, `current-worktree Vite on ${devServerHost}:${devServerPort}`);
}

async function ensureFreshDevServer() {
  const portPids = devServerListenerPids();
  const foreignPids = portPids.filter((pid) => !isCurrentProjectProcess(pid));
  if (foreignPids.length > 0) {
    throw new Error(`Port ${devServerPort} is already used by non-TOKENICODE pids: ${foreignPids.join(', ')}`);
  }

  const stopped = await stopCurrentWorktreeDevServer();
  if (stopped.survivors.length > 0) {
    throw new Error(`Could not stop stale Vite pids: ${stopped.survivors.join(', ')}`);
  }

  log(`starting alpha Vite on ${devServerHost}:${devServerPort}`);
  const child = spawn(
    'pnpm',
    ['exec', 'vite', '--host', devServerHost, '--port', String(devServerPort), '--strictPort'],
    {
      cwd: projectRoot,
      detached: true,
      stdio: 'ignore',
      env: cuaEnv(),
    },
  );
  child.unref();

  const ready = await waitFor(devServerReady, 30_000, 500);
  if (!ready) {
    throw new Error(`Vite did not become ready on ${devServerHost}:${devServerPort}`);
  }
  return { started: true, pid: child.pid, stopped };
}

function cString(value) {
  return JSON.stringify(value);
}

function writeNativeLauncher() {
  const source = `#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

extern char **environ;

static void fail(const char *message) {
  fprintf(stderr, "[launch-cua-dev] %s: %s\\n", message, strerror(errno));
  fflush(stderr);
  _exit(127);
}

static void set_env_or_die(const char *key, const char *value) {
  if (setenv(key, value, 1) != 0) {
    fail("setenv failed");
  }
}

int main(void) {
  int log_fd = open(${cString(logPath)}, O_WRONLY | O_CREAT | O_TRUNC, 0644);
  if (log_fd < 0) {
    fail("open log failed");
  }
  if (dup2(log_fd, STDOUT_FILENO) < 0 || dup2(log_fd, STDERR_FILENO) < 0) {
    fail("redirect log failed");
  }
  if (log_fd > STDERR_FILENO) {
    close(log_fd);
  }

  if (chdir(${cString(tauriDir)}) != 0) {
    fail("chdir failed");
  }

  set_env_or_die("EDITION", "alpha");
  set_env_or_die("TOKENICODE_CUA_DEV", "1");

  char *const argv[] = { ${cString(currentBin)}, NULL };
  execve(${cString(currentBin)}, argv, environ);
  fail("execve failed");
}
`;
  writeFileSync(launcherSourcePath, source);

  const hostArch = capture('uname', ['-m']).trim();
  const archArgs = hostArch === 'arm64' ? ['-arch', 'arm64'] : [];
  run('cc', [...archArgs, '-Wall', '-Wextra', '-O2', '-o', launcherPath, launcherSourcePath], {
    cwd: projectRoot,
  });
  chmodSync(launcherPath, 0o755);
}

function signWrapper() {
  try {
    log(`codesign --force --deep --sign - ${wrapperApp}`);
    execFileSync(
      'codesign',
      ['--force', '--deep', '--sign', '-', '--timestamp=none', wrapperApp],
      { cwd: projectRoot, stdio: 'ignore', env: cuaEnv() },
    );
    return { signed: true, identity: 'adhoc' };
  } catch {
    log('ad-hoc codesign failed; continuing with unsigned debug wrapper');
    return { signed: false, identity: null };
  }
}

function writeWrapperApp() {
  if (!existsSync(sourceBin)) throw new Error(`Missing debug binary: ${sourceBin}`);

  rmSync(wrapperApp, { recursive: true, force: true });
  mkdirSync(join(wrapperApp, 'Contents', 'MacOS'), { recursive: true });

  copyFileSync(sourceBin, currentBin);
  chmodSync(currentBin, 0o755);

  const plist = `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>
  <string>${appName}</string>
  <key>CFBundleDisplayName</key>
  <string>${appName}</string>
  <key>CFBundleIdentifier</key>
  <string>${bundleId}</string>
  <key>CFBundleExecutable</key>
  <string>${launcherName}</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>LSMinimumSystemVersion</key>
  <string>10.13</string>
</dict>
</plist>
`;
  writeFileSync(join(wrapperApp, 'Contents', 'Info.plist'), plist);

  writeNativeLauncher();
  return signWrapper();
}

function registerWrapperWithLaunchServices() {
  if (!existsSync(launchServicesRegister)) {
    log('lsregister not found; relying on open to register the dev wrapper');
    return;
  }
  run(launchServicesRegister, ['-f', wrapperApp], { cwd: projectRoot, stdio: 'ignore' });
}

function launchWrapper() {
  run('open', ['-n', wrapperApp], { cwd: projectRoot, stdio: 'ignore' });
}

function currentBinPids() {
  return listProcesses()
    .filter((proc) => commandStartsWith(proc.command, currentBin))
    .map((proc) => proc.pid);
}

function processWindowTitles(pid) {
  const raw = capture('osascript', [
    '-e',
    `tell application "System Events" to tell (first process whose unix id is ${pid}) to get title of every window`,
  ]);
  return raw.split(',').map((title) => title.trim()).filter(Boolean);
}

function processBundleId(pid) {
  return capture('osascript', [
    '-e',
    `tell application "System Events" to tell (first process whose unix id is ${pid}) to get bundle identifier`,
  ]).trim();
}

async function verifyLaunch() {
  const pid = await waitFor(() => {
    const pids = currentBinPids();
    return pids.length === 1 ? pids[0] : null;
  }, 15_000, 250);
  if (!pid) throw new Error(`Expected exactly one current dev process for ${currentBin}`);

  const windowTitles = await waitFor(() => {
    const titles = processWindowTitles(pid);
    return titles.some((title) => title.includes(appName)) ? titles : null;
  }, 15_000, 250);
  if (!windowTitles) throw new Error(`Dev app process ${pid} did not expose a ${appName} window`);

  const bundle = processBundleId(pid);
  if (bundle !== bundleId) {
    throw new Error(`Expected bundle id ${bundleId}, got ${bundle || '<missing>'}`);
  }

  const devLogReady = await waitFor(() => readText(logPath).includes('[TOKENICODE]'), 10_000, 250);
  return { pid, bundleId: bundle, windowTitles, devLogReady: !!devLogReady };
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));

  const stopped = await stopOldCuaDevInstances();
  const devServer = await ensureFreshDevServer();

  if (opts.build) {
    run('cargo', ['build', '--no-default-features'], { cwd: tauriDir });
  }

  const wrapper = writeWrapperApp();
  registerWrapperWithLaunchServices();
  if (opts.open) launchWrapper();

  const verification = opts.open ? await verifyLaunch() : null;
  process.stdout.write(JSON.stringify({
    ok: true,
    appName,
    appPath: wrapperApp,
    bundleId,
    executable: currentBin,
    devUrl: `http://${devServerHost}:${devServerPort}`,
    logPath,
    stopped,
    devServer,
    wrapper,
    verification,
    computerUseTarget: wrapperApp,
  }, null, 2) + '\n');
}

main().catch((error) => {
  console.error(`[launch-cua-dev] ${error.stack || error.message || error}`);
  process.exit(1);
});
