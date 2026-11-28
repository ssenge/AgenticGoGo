//! The engine instance moves between threads and is shared by reference: any Rust parallelism (std threads, rayon,
//! tokio's blocking pool) can hold forks of it.

use relentless::prelude::*;

fn assert_send_sync<T: Send + Sync>() {}

/// `Relentless` is `Send + Sync`: this fails to compile when a field stops being.
#[test]
fn the_engine_instance_is_send_and_sync() {
    assert_send_sync::<Relentless>();
}
