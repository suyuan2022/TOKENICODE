/**
 * 组间拖拽落点 → 拖拽后「完整的有序组 id 列表」（整体喂给 groupStore.reorderGroups）。
 *
 * 纯函数：dnd-kit 的 onDragEnd 给出 active / over 的 id，这里用 arrayMove 语义算出
 * 完整新顺序。抽成纯函数是为了能在 node 环境直接单测，避开在 jsdom 里模拟 pointer
 * 拖拽的已知困难（见 ADR-0002）。reorderGroups 要的就是完整有序列表（mp-review 约束）。
 *
 * - active === over：没移动，原样返回。
 * - active / over 任一不在列表里：无效落点，原样返回。
 * - 始终返回新数组，不改动入参。
 */
export function reorderByDragEnd(
  orderedIds: string[],
  activeId: string,
  overId: string,
): string[] {
  if (activeId === overId) return orderedIds.slice();
  const from = orderedIds.indexOf(activeId);
  const to = orderedIds.indexOf(overId);
  if (from === -1 || to === -1) return orderedIds.slice();
  const next = orderedIds.slice();
  const [moved] = next.splice(from, 1);
  next.splice(to, 0, moved);
  return next;
}
