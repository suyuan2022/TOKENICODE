import { describe, it, expect } from 'vitest';
import { reorderByDragEnd } from '../groupDnd';

// 组间拖拽：dnd-kit 的 onDragEnd 给出 active / over 的 id，
// reorderByDragEnd 把它算成「拖拽后完整的有序组 id 列表」，
// 再整体喂给 groupStore.reorderGroups（mp-review 约束：传完整有序列表）。
describe('reorderByDragEnd (组间拖拽重排)', () => {
  it('把组从末尾拖到开头，得到完整新顺序', () => {
    expect(reorderByDragEnd(['a', 'b', 'c'], 'c', 'a')).toEqual(['c', 'a', 'b']);
  });

  it('相邻两组交换顺序', () => {
    expect(reorderByDragEnd(['a', 'b', 'c'], 'a', 'b')).toEqual(['b', 'a', 'c']);
  });

  it('把开头的组拖到末尾', () => {
    expect(reorderByDragEnd(['a', 'b', 'c'], 'a', 'c')).toEqual(['b', 'c', 'a']);
  });

  it('拖到自己身上（active === over）保持原顺序', () => {
    expect(reorderByDragEnd(['a', 'b', 'c'], 'b', 'b')).toEqual(['a', 'b', 'c']);
  });

  it('over 不存在时保持原顺序', () => {
    expect(reorderByDragEnd(['a', 'b', 'c'], 'a', 'ghost')).toEqual(['a', 'b', 'c']);
  });

  it('active 不存在时保持原顺序', () => {
    expect(reorderByDragEnd(['a', 'b', 'c'], 'ghost', 'a')).toEqual(['a', 'b', 'c']);
  });

  it('返回新数组，不改动入参（不可变）', () => {
    const input = ['a', 'b', 'c'];
    const out = reorderByDragEnd(input, 'c', 'a');
    expect(input).toEqual(['a', 'b', 'c']); // 入参原样
    expect(out).not.toBe(input); // 返回的是新引用
  });
});
