use crate::task::AtomicWaker;
use alloc::sync::Arc;
use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::AtomicPtr;
use core::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release};

use super::abort::abort;
use super::task::Task;

/// 描述从队列中取出结果的情况
pub(super) enum Dequeue<Fut> {
    // 取到了数据
    Data(*const Task<Fut>),
    Empty,
    // 此时处于不一致情况 建议重试
    Inconsistent,
}

pub(super) struct ReadyToRunQueue<Fut> {
    // The waker of the task using `FuturesUnordered`.
    // waker对象是可以设置的
    pub(super) waker: AtomicWaker,

    // Head/tail of the readiness queue    队列的头尾节点
    pub(super) head: AtomicPtr<Task<Fut>>,
    pub(super) tail: UnsafeCell<*const Task<Fut>>,
    pub(super) stub: Arc<Task<Fut>>,
}

/// An MPSC queue into which the tasks containing the futures are inserted
/// whenever the future inside is scheduled for polling.
impl<Fut> ReadyToRunQueue<Fut> {
    /// The enqueue function from the 1024cores intrusive MPSC queue algorithm.
    pub(super) fn enqueue(&self, task: *const Task<Fut>) {
        unsafe {
            debug_assert!((*task).queued.load(Relaxed));

            // This action does not require any coordination
            // 因为这是最新一个任务 所以next为空指针
            (*task).next_ready_to_run.store(ptr::null_mut(), Relaxed);

            // Note that these atomic orderings come from 1024cores
            // 链表操作和 future_channel 一样  从尾指针开始用next一路连接
            let task = task as *mut _;
            let prev = self.head.swap(task, AcqRel);
            (*prev).next_ready_to_run.store(task, Release);
        }
    }

    /// The dequeue function from the 1024cores intrusive MPSC queue algorithm
    ///
    /// Note that this is unsafe as it required mutual exclusion (only one
    /// thread can call this) to be guaranteed elsewhere.
    pub(super) unsafe fn dequeue(&self) -> Dequeue<Fut> {
        let mut tail = *self.tail.get();
        let mut next = (*tail).next_ready_to_run.load(Acquire);

        // 代表队列中只有之前的存根任务 每当队列中没有其他任务时 该对象就会自动回到队列中
        if tail == self.stub() {
            // 代表读完了所有元素
            if next.is_null() {
                return Dequeue::Empty;
            }

            // 有别的任务 更新tail 也可以理解为跳过了stub  否则应该是先返回stub的
            *self.tail.get() = next;
            tail = next;
            next = (*next).next_ready_to_run.load(Acquire);
        }

        // 这是最简单的情况 next不为空 直接返回
        if !next.is_null() {
            *self.tail.get() = next;
            debug_assert!(tail != self.stub());
            return Dequeue::Data(tail);
        }

        // next为空 首尾应当一致   否则返回Inconsistent 建议重试
        if self.head.load(Acquire) as *const _ != tail {
            return Dequeue::Inconsistent;
        }

        // 首尾一致时 会将一个stub插入 也就是队列中至少会有这个任务
        self.enqueue(self.stub());

        next = (*tail).next_ready_to_run.load(Acquire);

        // 这时就把stub返回
        if !next.is_null() {
            *self.tail.get() = next;
            return Dequeue::Data(tail);
        }

        Dequeue::Inconsistent
    }

    pub(super) fn stub(&self) -> *const Task<Fut> {
        Arc::as_ptr(&self.stub)
    }

    // Clear the queue of tasks.
    //
    // Note that each task has a strong reference count associated with it
    // which is owned by the ready to run queue. This method just pulls out
    // tasks and drops their refcounts.
    //
    // # Safety
    //
    // - All tasks **must** have had their futures dropped already (by FuturesUnordered::clear)
    // - The caller **must** guarantee unique access to `self`
    // 此时应该只有drop线程调用 也就不会出现不一致的情况
    pub(crate) unsafe fn clear(&self) {
        loop {
            // SAFETY: We have the guarantee of mutual exclusion required by `dequeue`.
            match self.dequeue() {
                Dequeue::Empty => break,
                Dequeue::Inconsistent => abort("inconsistent in drop"),
                Dequeue::Data(ptr) => drop(Arc::from_raw(ptr)),
            }
        }
    }
}

impl<Fut> Drop for ReadyToRunQueue<Fut> {
    fn drop(&mut self) {
        // Once we're in the destructor for `Inner<Fut>` we need to clear out
        // the ready to run queue of tasks if there's anything left in there.

        // All tasks have had their futures dropped already by the `FuturesUnordered`
        // destructor above, and we have &mut self, so this is safe.
        unsafe {
            self.clear();
        }
    }
}
