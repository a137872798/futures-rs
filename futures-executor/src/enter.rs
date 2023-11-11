use std::cell::Cell;
use std::fmt;

// 每个线程 维护一个本地变量
thread_local!(static ENTERED: Cell<bool> = Cell::new(false));

/// Represents an executor context.
///
/// For more details, see [`enter` documentation](enter()).
pub struct Enter {
    _priv: (),
}

/// An error returned by `enter` if an execution scope has already been
/// entered.
pub struct EnterError {
    _priv: (),
}

impl fmt::Debug for EnterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnterError").finish()
    }
}

impl fmt::Display for EnterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "an execution scope has already been entered")
    }
}

impl std::error::Error for EnterError {}

/// Marks the current thread as being within the dynamic extent of an
/// executor.
///
/// Executor implementations should call this function before beginning to
/// execute a task, and drop the returned [`Enter`](Enter) value after
/// completing task execution:
///
/// ```
/// use futures::executor::enter;
///
/// let enter = enter().expect("...");
/// /* run task */
/// drop(enter);
/// ```
///
/// Doing so ensures that executors aren't
/// accidentally invoked in a nested fashion.
///
/// # Error
///
/// Returns an error if the current thread is already marked, in which case the
/// caller should panic with a tailored error message.
///
/// 该方法不能重复调用 用于避免重入的情况
pub fn enter() -> Result<Enter, EnterError> {
    ENTERED.with(|c| {
        if c.get() {
            Err(EnterError { _priv: () })
        } else {
            c.set(true);

            Ok(Enter { _priv: () })
        }
    })
}

impl fmt::Debug for Enter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Enter").finish()
    }
}

/// 对象被释放时  设置为false
impl Drop for Enter {
    fn drop(&mut self) {
        ENTERED.with(|c| {
            assert!(c.get());
            c.set(false);
        });
    }
}
