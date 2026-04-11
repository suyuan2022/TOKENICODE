#!/bin/bash
# TOKENICODE 全量测试执行脚本 — 串行执行，无并发干扰
# 用法: bash .test/scripts/run-all-phases.sh [run-name]
# 示例: bash .test/scripts/run-all-phases.sh 2026-04-12-provider-regression
# 默认 run-name: 当天日期-full-run

set -uo pipefail
cd "$(dirname "$0")/../.."  # 回到项目根目录

RUNNER="node scripts/run-tests.mjs"
RUN_NAME=${1:-$(date '+%Y-%m-%d')-full-run}
RUN_DIR=".test/runs/$RUN_NAME"
mkdir -p "$RUN_DIR"

run_suite() {
  local suite_name=$1
  local suite_file=$2
  local rounds=$3
  local detail=${4:-standard}

  local suite_path=".test/suites/$suite_name/$suite_file"
  local report_dir="$RUN_DIR/$suite_name"
  mkdir -p "$report_dir"

  echo ""
  echo "================================================================"
  echo "  SUITE: $suite_name × $rounds rounds (detail: $detail)"
  echo "  Time: $(date '+%H:%M:%S')"
  echo "================================================================"

  for i in $(seq 1 $rounds); do
    local padded=$(printf '%03d' $i)
    local report="$report_dir/run-${padded}.json"
    # 跳过已存在的报告
    if [ -f "$report" ]; then
      echo "  [skip] run-${padded} already exists"
      continue
    fi

    # 每 5 轮 ping 检查一次
    if [ "$((i % 5))" -eq 1 ]; then
      local ping_result=$(node scripts/tokenicode-cli.mjs ping 2>&1 || echo '{"ok":false}')
      if ! echo "$ping_result" | grep -q '"pong":true'; then
        echo "  [WARN] App unresponsive at run-${padded}, attempting restart..."
        node scripts/tokenicode-cli.mjs restart --timeout 60000 2>&1 || true
        sleep 5
      fi
    fi

    echo -n "  [${padded}/${rounds}] "
    local summary=$($RUNNER "$suite_path" --report "$report" --detail "$detail" 2>&1 | grep -E '^\{.*_summary' || echo '{"_summary":true,"error":"no output"}')
    echo "$summary"

    # 检查是否全部 JS timeout（webview 冻结），如果是则 relaunch 恢复
    if [ -f "$report" ]; then
      local all_js_timeout=$(node -e "try{const r=require('./$report');const f=r.issues||[];const jt=f.filter(i=>i.type==='test_failure'&&i.error&&i.error.includes('Timeout waiting for JS execution'));if(jt.length>0&&jt.length===f.filter(i=>i.type==='test_failure').length)console.log('yes');else console.log('no')}catch(e){console.log('no')}" 2>/dev/null)
      if [ "$all_js_timeout" = "yes" ]; then
        echo "  [RECOVERY] All failures were JS timeout, relaunching app..."
        node scripts/tokenicode-cli.mjs relaunch --timeout 120000 2>&1 || true
        sleep 10
        # 验证恢复
        local check=$(node scripts/tokenicode-cli.mjs status 2>&1 || echo 'fail')
        if echo "$check" | grep -q '"ok":true'; then
          echo "  [RECOVERY] App recovered successfully"
        else
          echo "  [RECOVERY] Recovery failed, will continue anyway"
        fi
      fi
    fi
  done
}

echo "=========================================="
echo "  TOKENICODE Full Serial Test Run"
echo "  Run: $RUN_NAME"
echo "  Output: $RUN_DIR"
echo "  Started: $(date)"
echo "=========================================="

# Phase 1: 快速验证 (已完成的会被 skip)
echo ""
echo "############### PHASE 1: 快速验证 ###############"
run_suite "health-and-status" "full-health.json" 3 minimal
run_suite "settings-panel" "settings-operations.json" 3 minimal
run_suite "ui-state-checks" "ui-elements.json" 3 standard
run_suite "session-management" "session-ops.json" 5 minimal
run_suite "model-switching" "model-and-provider.json" 5 minimal

# Phase 2: 核心 bug
echo ""
echo "############### PHASE 2: 核心 bug ###############"
run_suite "basic-chat" "send-receive.json" 5 standard
run_suite "interrupt-recovery" "interrupt-then-send.json" 5 standard
run_suite "streaming-stress" "stream-stall.json" 5 standard
run_suite "message-during-stream" "msg-while-streaming.json" 5 standard

# Phase 3: 深度多轮
echo ""
echo "############### PHASE 3: 深度多轮 ###############"
run_suite "interrupt-recovery" "interrupt-then-send.json" 20 standard
run_suite "streaming-stress" "stream-stall.json" 20 standard
run_suite "message-during-stream" "msg-while-streaming.json" 20 standard
run_suite "basic-chat" "send-receive.json" 10 standard
run_suite "multi-session" "tab-switching.json" 10 standard
run_suite "provider-persistence" "provider-revert.json" 10 standard

# Phase 4: 收尾
echo ""
echo "############### PHASE 4: 收尾 ###############"
run_suite "permission-flow" "permission-handling.json" 5 standard
run_suite "slash-commands" "command-recognition.json" 5 standard
run_suite "restart-recovery" "restart-tests.json" 5 standard

echo ""
echo "=========================================="
echo "  All phases complete!"
echo "  Ended: $(date)"
echo "=========================================="

# 汇总分析
echo ""
echo "--- Running analysis ---"
python3 .test/scripts/analyze-reports.py "$RUN_DIR"
