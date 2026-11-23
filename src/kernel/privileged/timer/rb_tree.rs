//! 侵入式红黑树 — `TimerBase` 的有序容器
//!
//! 节点的存储由调用方持有 (`HrTimer` 内嵌 [`Node`])；树本身只保存根指针与长度，
//! 不分配、不复制节点。所有操作都要求调用方已持有 `TimerBase` 锁，且同一节点
//! 不能同时出现在两棵树中。
//!
//! 排序键由调用方写入 `Node::key`；定时器使用 `(expiry_ns, activation_seq)`，
//! 保证同到期时间下顺序稳定。

use core::ptr::NonNull;

/// 红黑树节点颜色
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RbColor {
    /// 红
    Red,
    /// 黑
    Black,
}

/// 侵入式红黑树节点
///
/// `K` 必须可比较且可复制；节点地址在插入后必须保持稳定（内嵌于 `HrTimer`）。
pub(super) struct RbNode<K> {
    pub(super) left: Option<NonNull<Self>>,
    pub(super) right: Option<NonNull<Self>>,
    pub(super) parent: Option<NonNull<Self>>,
    pub(super) color: RbColor,
    pub(super) key: K,
}

impl<K> RbNode<K> {
    /// 构造未链接的红色节点 (根会在插入修正中变黑)
    pub(super) const fn new(key: K) -> Self {
        Self {
            left: None,
            right: None,
            parent: None,
            color: RbColor::Red,
            key,
        }
    }

    /// 清空链接; 不读取 key
    pub(super) fn reset(&mut self) {
        self.left = None;
        self.right = None;
        self.parent = None;
        self.color = RbColor::Red;
    }
}

/// 侵入式红黑树
pub(super) struct RbTree<K> {
    root: Option<NonNull<RbNode<K>>>,
    len: usize,
}

impl<K> RbTree<K> {
    /// 空树
    pub(super) const fn new() -> Self {
        Self { root: None, len: 0 }
    }

    /// 节点数
    pub(super) const fn len(&self) -> usize {
        self.len
    }

    /// 树是否为空
    pub(super) const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 清空树 (不解引用节点; 调用方保证节点生命周期)
    pub(super) fn clear(&mut self) {
        self.root = None;
        self.len = 0;
    }
}

/// 删除修正一步的结果: 新 x / 新父 / x 是否位于父的左槽.
type FixupStep<K> = (Option<NonNull<RbNode<K>>>, Option<NonNull<RbNode<K>>>, bool);

impl<K: Copy + Ord> RbTree<K> {
    /// 插入节点
    ///
    /// # Safety
    ///
    /// - `node` 指向有效、地址稳定的 `RbNode`，且当前不在任何树中；
    /// - 调用方持有树的独占访问权 (同一把 `TimerBase` 锁)；
    /// - 插入后到移除前，节点必须保持有效且不可移动。
    pub(super) unsafe fn insert(&mut self, node: NonNull<RbNode<K>>) {
        let mut parent: Option<NonNull<RbNode<K>>> = None;
        let mut current = self.root;

        // SAFETY: node 由树 API 传入且属于本树; 本方法只在本树临界区内解引用.
        let key = unsafe { (*node.as_ptr()).key };

        while let Some(cur) = current {
            parent = Some(cur);
            // SAFETY: cur 来自树根或上一节点的链接, 在本树临界区内逐节点有效.
            let cur_node = unsafe { &*cur.as_ptr() };
            if key < cur_node.key {
                current = cur_node.left;
            } else {
                current = cur_node.right;
            }
        }

        // SAFETY: node 有效且未链接; parent 来自本树.
        unsafe {
            let node_ref = &mut *node.as_ptr();
            node_ref.left = None;
            node_ref.right = None;
            node_ref.parent = parent;
            node_ref.color = RbColor::Red;
        }

        if let Some(p) = parent {
            // SAFETY: p 是树中节点; 比较后挂到空的一侧.
            unsafe {
                let p_ref = &mut *p.as_ptr();
                if key < p_ref.key {
                    p_ref.left = Some(node);
                } else {
                    p_ref.right = Some(node);
                }
            }
        } else {
            self.root = Some(node);
        }

        self.len += 1;
        // SAFETY: node 已在树中, 祖先链完整.
        unsafe { self.insert_fixup(node) };
    }

    /// 移除已插入的节点
    ///
    /// # Safety
    ///
    /// - `node` 必须在本树中，且地址稳定；
    /// - 调用方持有树的独占访问权；
    /// - 返回后节点链接已清空，可重新插入另一棵树。
    pub(super) unsafe fn remove(&mut self, node: NonNull<RbNode<K>>) {
        let z = node.as_ptr();

        // SAFETY: node 已由插入路径挂入本树, 本次临界区内保持链接有效.
        let mut removed_color = unsafe { (*z).color };
        let x: Option<NonNull<RbNode<K>>>;
        let x_parent: Option<NonNull<RbNode<K>>>;
        let x_is_left: bool;

        // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
        let left = unsafe { (*z).left };
        // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
        let right = unsafe { (*z).right };
        // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
        let z_is_left = unsafe {
            (*z).parent
                .is_some_and(|p| (*p.as_ptr()).left == Some(node))
        };

        if left.is_none() {
            x = right;
            // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
            x_parent = unsafe { (*z).parent };
            x_is_left = z_is_left;
            // SAFETY: z 在树中, transplant 只改链接.
            unsafe { self.transplant(node, right) };
        } else if right.is_none() {
            x = left;
            // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
            x_parent = unsafe { (*z).parent };
            x_is_left = z_is_left;
            // SAFETY: 同上.
            unsafe { self.transplant(node, left) };
        } else {
            // 两个孩子: 用后继 y 顶替 z 的位置.
            // SAFETY: right 非空, minimum 沿树内链接行走.
            let y = unsafe { Self::minimum(right.expect("right checked")) };
            // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
            removed_color = unsafe { (*y.as_ptr()).color };
            // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
            x = unsafe { (*y.as_ptr()).right };

            // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
            if unsafe { (*y.as_ptr()).parent } == Some(node) {
                x_parent = Some(y);
                x_is_left = false;
                if let Some(xx) = x {
                    // SAFETY: xx 是 y 的右孩子, y 在树中.
                    unsafe { (*xx.as_ptr()).parent = Some(y) };
                }
            } else {
                // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
                let y_parent = unsafe { (*y.as_ptr()).parent };
                x_parent = y_parent;
                // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
                x_is_left = y_parent.is_some_and(|p| unsafe { (*p.as_ptr()).left == Some(y) });
                // SAFETY: y 在 z 的右子树中, transplant 保持树结构.
                unsafe { self.transplant(y, x) };
                // SAFETY: y 接管 z 的右子树.
                unsafe {
                    (*y.as_ptr()).right = right;
                    (*right.expect("right checked").as_ptr()).parent = Some(y);
                }
            }

            // SAFETY: y 已从原位置摘除, 现在顶替 z.
            unsafe { self.transplant(node, Some(y)) };
            // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
            unsafe {
                (*y.as_ptr()).left = left;
                if let Some(left) = left {
                    (*left.as_ptr()).parent = Some(y);
                }
                (*y.as_ptr()).color = (*z).color;
            }
        }

        // SAFETY: z 已不在树中; 清空其链接, 防止误用.
        unsafe {
            let z_ref = &mut *z;
            z_ref.left = None;
            z_ref.right = None;
            z_ref.parent = None;
            z_ref.color = RbColor::Red;
        }

        self.len -= 1;
        if removed_color == RbColor::Black {
            // SAFETY: x 是替代 z 的子树根 (可能为空), x_parent 是其在树中的父.
            unsafe { self.delete_fixup(x, x_parent, x_is_left) };
        }
    }

    /// 最早节点 (中序最小)
    ///
    /// # Safety
    ///
    /// 树中所有节点必须在调用期间有效。
    pub(super) unsafe fn first(&self) -> Option<NonNull<RbNode<K>>> {
        // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
        self.root.map(|root| unsafe { Self::minimum(root) })
    }

    /// 中序后继
    ///
    /// # Safety
    ///
    /// `node` 必须在本树中，树中节点在调用期间有效。
    pub(super) unsafe fn next(node: NonNull<RbNode<K>>) -> Option<NonNull<RbNode<K>>> {
        let n = node.as_ptr();
        // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
        let right = unsafe { (*n).right };
        if let Some(right) = right {
            // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
            return Some(unsafe { Self::minimum(right) });
        }

        let mut current = node;
        // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
        let mut parent = unsafe { (*n).parent };
        while let Some(p) = parent {
            // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
            if unsafe { (*p.as_ptr()).left } == Some(current) {
                return Some(p);
            }
            current = p;
            // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
            parent = unsafe { (*p.as_ptr()).parent };
        }
        None
    }

    /// 子树最小节点
    ///
    /// # Safety
    ///
    /// `node` 及其左链节点必须在调用期间有效。
    unsafe fn minimum(mut node: NonNull<RbNode<K>>) -> NonNull<RbNode<K>> {
        let mut guard = 0usize;
        loop {
            guard += 1;
            assert!(guard < 4096, "rb minimum cycle");
            // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
            let left = unsafe { (*node.as_ptr()).left };
            match left {
                Some(left) => node = left,
                None => return node,
            }
        }
    }

    /// 用子树 `v` 顶替 `u` 的位置
    ///
    /// # Safety
    ///
    /// `u` 在本树中；`v` 可为空或为树内子树。
    unsafe fn transplant(&mut self, u: NonNull<RbNode<K>>, v: Option<NonNull<RbNode<K>>>) {
        // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
        let parent = unsafe { (*u.as_ptr()).parent };
        if let Some(p) = parent {
            // SAFETY: p 在树中; u 是 p 的左或右孩子.
            unsafe {
                if (*p.as_ptr()).left == Some(u) {
                    (*p.as_ptr()).left = v;
                } else {
                    (*p.as_ptr()).right = v;
                }
            }
        } else {
            self.root = v;
        }
        if let Some(v) = v {
            // SAFETY: v 是本树内子树根.
            unsafe { (*v.as_ptr()).parent = parent };
        }
    }

    /// 插入修正
    ///
    /// # Safety
    ///
    /// `z` 已插入本树，祖先链接完整。
    unsafe fn insert_fixup(&mut self, mut z: NonNull<RbNode<K>>) {
        let mut guard = 0usize;
        // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
        while let Some(parent) = unsafe { (*z.as_ptr()).parent } {
            guard += 1;
            assert!(guard < 4096, "rb insert fixup cycle");
            // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
            if unsafe { (*parent.as_ptr()).color } == RbColor::Black {
                break;
            }
            // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
            let grand = unsafe {
                (*parent.as_ptr())
                    .parent
                    .expect("red parent must have a grandparent")
            };

            // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
            if unsafe { (*grand.as_ptr()).left } == Some(parent) {
                // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
                let uncle = unsafe { (*grand.as_ptr()).right };
                if Self::is_red(uncle) {
                    // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
                    unsafe {
                        (*parent.as_ptr()).color = RbColor::Black;
                        (*uncle.expect("red uncle").as_ptr()).color = RbColor::Black;
                        (*grand.as_ptr()).color = RbColor::Red;
                    }
                    z = grand;
                } else {
                    // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
                    if unsafe { (*parent.as_ptr()).right } == Some(z) {
                        z = parent;
                        // SAFETY: z 在树中, 左旋保持树结构.
                        unsafe { self.left_rotate(z) };
                    }
                    // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
                    let parent = unsafe {
                        (*z.as_ptr())
                            .parent
                            .expect("rotated node must have a parent")
                    };
                    // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
                    let grand = unsafe {
                        (*parent.as_ptr())
                            .parent
                            .expect("red parent must have a grandparent")
                    };
                    // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
                    unsafe {
                        (*parent.as_ptr()).color = RbColor::Black;
                        (*grand.as_ptr()).color = RbColor::Red;
                        self.right_rotate(grand);
                    }
                }
            } else {
                // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
                let uncle = unsafe { (*grand.as_ptr()).left };
                if Self::is_red(uncle) {
                    // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
                    unsafe {
                        (*parent.as_ptr()).color = RbColor::Black;
                        (*uncle.expect("red uncle").as_ptr()).color = RbColor::Black;
                        (*grand.as_ptr()).color = RbColor::Red;
                    }
                    z = grand;
                } else {
                    // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
                    if unsafe { (*parent.as_ptr()).left } == Some(z) {
                        z = parent;
                        // SAFETY: 对称情形, 右旋保持树结构.
                        unsafe { self.right_rotate(z) };
                    }
                    // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
                    let parent = unsafe {
                        (*z.as_ptr())
                            .parent
                            .expect("rotated node must have a parent")
                    };
                    // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
                    let grand = unsafe {
                        (*parent.as_ptr())
                            .parent
                            .expect("red parent must have a grandparent")
                    };
                    // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
                    unsafe {
                        (*parent.as_ptr()).color = RbColor::Black;
                        (*grand.as_ptr()).color = RbColor::Red;
                        self.left_rotate(grand);
                    }
                }
            }
        }

        if let Some(root) = self.root {
            // SAFETY: root 在树中.
            unsafe { (*root.as_ptr()).color = RbColor::Black };
        }
    }

    /// 删除修正
    ///
    /// # Safety
    ///
    /// `x` 是刚被摘除节点的替代子树根 (可为空); `parent` 是 `x` 在树中的父；
    /// `x_is_left` 表示 `x` 占据的是父节点的左槽 (空 `x` 时不能靠指针比较判定)。
    unsafe fn delete_fixup(
        &mut self,
        mut x: Option<NonNull<RbNode<K>>>,
        mut parent: Option<NonNull<RbNode<K>>>,
        mut x_is_left: bool,
    ) {
        while x != self.root && Self::is_black(x) {
            let Some(p) = parent else {
                break;
            };
            // SAFETY: p 在树中; 左右分支各自维护旋转后的树结构.
            let (next_x, next_parent, next_left) = if x_is_left {
                // SAFETY: p 在树中; 左分支维护旋转后的树结构.
                unsafe { self.delete_fixup_left(p) }
            } else {
                // SAFETY: p 在树中; 右分支维护旋转后的树结构.
                unsafe { self.delete_fixup_right(p) }
            };
            x = next_x;
            parent = next_parent;
            x_is_left = next_left;
        }
        if let Some(x) = x {
            // SAFETY: x 在树中.
            unsafe { (*x.as_ptr()).color = RbColor::Black };
        }
    }

    /// 删除修正左分支: `x` 是 `p` 的左孩子.
    unsafe fn delete_fixup_left(&mut self, p: NonNull<RbNode<K>>) -> FixupStep<K> {
        // SAFETY: p 在树中.
        let mut w = unsafe { (*p.as_ptr()).right };
        if let Some(wn) = w {
            // SAFETY: w 的节点在树中.
            if unsafe { (*wn.as_ptr()).color } == RbColor::Red {
                // SAFETY: 兄弟为红: 反色并左旋 p.
                unsafe {
                    (*wn.as_ptr()).color = RbColor::Black;
                    (*p.as_ptr()).color = RbColor::Red;
                    self.left_rotate(p);
                }
                // SAFETY: 旋转后重新读 p 的新右孩子.
                w = unsafe { (*p.as_ptr()).right };
            }
        }
        let Some(mut w) = w else {
            // 不完整树只可能来自调用方违约; 向上收敛, 不 deref 空兄弟.
            // SAFETY: p 在树中.
            return unsafe { Self::delete_fixup_parent_side(p) };
        };
        // SAFETY: w 在树中, 读其两个孩子颜色.
        if Self::is_black(unsafe { (*w.as_ptr()).left })
            // SAFETY: w 在树中.
            && Self::is_black(unsafe { (*w.as_ptr()).right })
        {
            // 兄弟双黑: 染红并向 p 收敛.
            // SAFETY: w 在树中.
            unsafe { (*w.as_ptr()).color = RbColor::Red };
            // SAFETY: p 在树中.
            return unsafe { Self::delete_fixup_parent_side(p) };
        }
        // SAFETY: w 在树中.
        if Self::is_black(unsafe { (*w.as_ptr()).right }) {
            // SAFETY: 近侄为红: 反色并右旋 w.
            unsafe {
                if let Some(left) = (*w.as_ptr()).left {
                    (*left.as_ptr()).color = RbColor::Black;
                }
                (*w.as_ptr()).color = RbColor::Red;
                self.right_rotate(w);
            }
            // SAFETY: 旋转后重新读 p 的新右孩子.
            // SAFETY: p 在树中, 旋转后读新右孩子.
            let Some(new_w) = (unsafe { (*p.as_ptr()).right }) else {
                return (self.root, None, false);
            };
            w = new_w;
        }
        // SAFETY: 远侄为红: 反色、左旋 p, 收敛到根.
        unsafe {
            (*w.as_ptr()).color = (*p.as_ptr()).color;
            (*p.as_ptr()).color = RbColor::Black;
            if let Some(right) = (*w.as_ptr()).right {
                (*right.as_ptr()).color = RbColor::Black;
            }
            self.left_rotate(p);
        }
        (self.root, None, false)
    }

    /// 删除修正右分支: `x` 是 `p` 的右孩子.
    unsafe fn delete_fixup_right(&mut self, p: NonNull<RbNode<K>>) -> FixupStep<K> {
        // SAFETY: p 在树中.
        let mut w = unsafe { (*p.as_ptr()).left };
        if let Some(wn) = w {
            // SAFETY: w 的节点在树中.
            if unsafe { (*wn.as_ptr()).color } == RbColor::Red {
                // SAFETY: 兄弟为红: 反色并右旋 p.
                unsafe {
                    (*wn.as_ptr()).color = RbColor::Black;
                    (*p.as_ptr()).color = RbColor::Red;
                    self.right_rotate(p);
                }
                // SAFETY: 旋转后重新读 p 的新左孩子.
                w = unsafe { (*p.as_ptr()).left };
            }
        }
        let Some(mut w) = w else {
            // SAFETY: p 在树中.
            return unsafe { Self::delete_fixup_parent_side(p) };
        };
        // SAFETY: w 在树中, 读其两个孩子颜色.
        if Self::is_black(unsafe { (*w.as_ptr()).left })
            // SAFETY: w 在树中.
            && Self::is_black(unsafe { (*w.as_ptr()).right })
        {
            // SAFETY: w 在树中.
            unsafe { (*w.as_ptr()).color = RbColor::Red };
            // SAFETY: p 在树中.
            return unsafe { Self::delete_fixup_parent_side(p) };
        }
        // SAFETY: w 在树中.
        if Self::is_black(unsafe { (*w.as_ptr()).left }) {
            // SAFETY: 近侄为红: 反色并左旋 w.
            unsafe {
                if let Some(right) = (*w.as_ptr()).right {
                    (*right.as_ptr()).color = RbColor::Black;
                }
                (*w.as_ptr()).color = RbColor::Red;
                self.left_rotate(w);
            }
            // SAFETY: 旋转后重新读 p 的新左孩子.
            // SAFETY: p 在树中, 旋转后读新左孩子.
            let Some(new_w) = (unsafe { (*p.as_ptr()).left }) else {
                return (self.root, None, false);
            };
            w = new_w;
        }
        // SAFETY: 远侄为红: 反色、右旋 p, 收敛到根.
        unsafe {
            (*w.as_ptr()).color = (*p.as_ptr()).color;
            (*p.as_ptr()).color = RbColor::Black;
            if let Some(left) = (*w.as_ptr()).left {
                (*left.as_ptr()).color = RbColor::Black;
            }
            self.right_rotate(p);
        }
        (self.root, None, false)
    }

    /// 从 `p` 读父节点与所在侧, 用于双黑上溯.
    unsafe fn delete_fixup_parent_side(p: NonNull<RbNode<K>>) -> FixupStep<K> {
        // SAFETY: p 在树中.
        let parent = unsafe { (*p.as_ptr()).parent };
        // SAFETY: parent 在树中.
        let is_left = parent.is_some_and(|pp| unsafe { (*pp.as_ptr()).left == Some(p) });
        (Some(p), parent, is_left)
    }

    /// 左旋
    ///
    /// # Safety
    ///
    /// `x` 在树中且其右孩子非空。
    unsafe fn left_rotate(&mut self, x: NonNull<RbNode<K>>) {
        // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
        let y = unsafe { (*x.as_ptr()).right.expect("left_rotate requires right") };
        // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
        unsafe {
            (*x.as_ptr()).right = (*y.as_ptr()).left;
            if let Some(left) = (*y.as_ptr()).left {
                (*left.as_ptr()).parent = Some(x);
            }
            (*y.as_ptr()).parent = (*x.as_ptr()).parent;
            if let Some(parent) = (*x.as_ptr()).parent {
                if (*parent.as_ptr()).left == Some(x) {
                    (*parent.as_ptr()).left = Some(y);
                } else {
                    (*parent.as_ptr()).right = Some(y);
                }
            } else {
                self.root = Some(y);
            }
            (*y.as_ptr()).left = Some(x);
            (*x.as_ptr()).parent = Some(y);
        }
    }

    /// 右旋
    ///
    /// # Safety
    ///
    /// `x` 在树中且其左孩子非空。
    unsafe fn right_rotate(&mut self, x: NonNull<RbNode<K>>) {
        // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
        let y = unsafe { (*x.as_ptr()).left.expect("right_rotate requires left") };
        // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
        unsafe {
            (*x.as_ptr()).left = (*y.as_ptr()).right;
            if let Some(right) = (*y.as_ptr()).right {
                (*right.as_ptr()).parent = Some(x);
            }
            (*y.as_ptr()).parent = (*x.as_ptr()).parent;
            if let Some(parent) = (*x.as_ptr()).parent {
                if (*parent.as_ptr()).right == Some(x) {
                    (*parent.as_ptr()).right = Some(y);
                } else {
                    (*parent.as_ptr()).left = Some(y);
                }
            } else {
                self.root = Some(y);
            }
            (*y.as_ptr()).right = Some(x);
            (*x.as_ptr()).parent = Some(y);
        }
    }

    fn is_red(node: Option<NonNull<RbNode<K>>>) -> bool {
        match node {
            // SAFETY: node 取自树内链接, 在本次临界区内链接保持有效.
            Some(n) => (unsafe { (*n.as_ptr()).color }) == RbColor::Red,
            None => false,
        }
    }

    fn is_black(node: Option<NonNull<RbNode<K>>>) -> bool {
        !Self::is_red(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::boxed::Box;
    use alloc::vec::Vec;

    /// 校验根为黑、无红红、黑高一致、parent 链接一致.
    unsafe fn validate_node<K: Copy + Ord + core::fmt::Debug>(
        node: Option<NonNull<RbNode<K>>>,
        parent: Option<NonNull<RbNode<K>>>,
    ) -> usize {
        let Some(n) = node else {
            return 1;
        };
        // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
        assert_eq!(unsafe { (*n.as_ptr()).parent }, parent);
        // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
        let left = unsafe { (*n.as_ptr()).left };
        // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
        let right = unsafe { (*n.as_ptr()).right };
        // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
        if unsafe { (*n.as_ptr()).color } == RbColor::Red {
            assert!(!RbTree::<K>::is_red(left), "红节点左孩子为红");
            assert!(!RbTree::<K>::is_red(right), "红节点右孩子为红");
        }
        // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
        let lh = unsafe { validate_node(left, Some(n)) };
        // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
        let rh = unsafe { validate_node(right, Some(n)) };
        assert_eq!(lh, rh, "黑高不一致");
        // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
        lh + usize::from(unsafe { (*n.as_ptr()).color } == RbColor::Black)
    }

    unsafe fn validate<K: Copy + Ord + core::fmt::Debug>(tree: &RbTree<K>) {
        if let Some(root) = tree.root {
            // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
            assert_eq!(unsafe { (*root.as_ptr()).color }, RbColor::Black);
            // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
            let _ = unsafe { validate_node(Some(root), None) };
        } else {
            assert_eq!(tree.len(), 0);
        }
        let mut count = 0;
        // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
        let mut cur = unsafe { tree.first() };
        while let Some(n) = cur {
            count += 1;
            // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
            let next = unsafe { RbTree::next(n) };
            if let Some(next) = next {
                // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
                assert!(unsafe { (*n.as_ptr()).key } <= unsafe { (*next.as_ptr()).key });
            }
            cur = next;
        }
        assert_eq!(count, tree.len());
    }

    #[test]
    fn insert_and_remove_all_orders() {
        for count in 0..32u64 {
            let nodes: Vec<Box<RbNode<u64>>> = (0..count)
                .map(|i| Box::new(RbNode::new((i * 17) % 31 + 1)))
                .collect();
            let mut tree = RbTree::new();
            let mut ptrs = Vec::new();
            for node in &nodes {
                let ptr = NonNull::from(node.as_ref());
                // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
                unsafe { tree.insert(ptr) };
                ptrs.push(ptr);
            }
            // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
            unsafe { validate(&tree) };
            assert_eq!(tree.len(), nodes.len());

            // 正序删除
            for ptr in ptrs {
                // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
                unsafe { tree.remove(ptr) };
                // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
                unsafe { validate(&tree) };
            }
            assert_eq!(tree.len(), 0);
        }
    }

    #[test]
    fn remove_reverse_order() {
        let nodes: Vec<Box<RbNode<u64>>> = (1..=16).map(|i| Box::new(RbNode::new(i))).collect();
        let mut tree = RbTree::new();
        let mut ptrs = Vec::new();
        for node in &nodes {
            let ptr = NonNull::from(node.as_ref());
            // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
            unsafe { tree.insert(ptr) };
            ptrs.push(ptr);
        }
        // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
        unsafe { validate(&tree) };
        for ptr in ptrs.into_iter().rev() {
            // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
            unsafe { tree.remove(ptr) };
            // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
            unsafe { validate(&tree) };
        }
        assert_eq!(tree.len(), 0);
    }

    #[test]
    fn remove_middle_random_order() {
        let nodes: Vec<Box<RbNode<u64>>> = (1..=24).map(|i| Box::new(RbNode::new(i * 3))).collect();
        let mut tree = RbTree::new();
        let ptrs: Vec<_> = nodes.iter().map(|n| NonNull::from(n.as_ref())).collect();
        for &ptr in &ptrs {
            // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
            unsafe { tree.insert(ptr) };
        }
        // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
        unsafe { validate(&tree) };
        for idx in [
            11, 2, 19, 5, 0, 23, 7, 15, 1, 22, 9, 18, 4, 12, 21, 3, 20, 6, 10, 8,
        ] {
            // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
            unsafe { tree.remove(ptrs[idx]) };
            // SAFETY: 节点属于本树且本次临界区独占树结构, 解引用只读取 `RbNode` 字段, 不转移所有权.
            unsafe { validate(&tree) };
        }
        assert_eq!(tree.len(), 4);
    }
}
