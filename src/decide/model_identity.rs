//! The last synchronous request on this thread, retained until its consumer records it.
use std::cell::RefCell;

thread_local! {
    static REQUESTED: RefCell<Option<String>> = const { RefCell::new(None) };
}

pub fn begin(model: Option<String>) {
    REQUESTED.with(|m| *m.borrow_mut() = model);
}

pub fn requested() -> Option<String> {
    REQUESTED.with(|m| m.borrow().clone())
}
