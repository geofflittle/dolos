use std::{sync::mpsc, time::Duration};

use dolos_core::{Domain as _, MempoolStore as _, SubmitExt as _};
use dolos_testing::toy_domain::ToyDomain;

/// Whether `submitter` finishes a submission while `holder` holds its submit
/// lock.
fn submits_while_held(holder: &ToyDomain, submitter: &ToyDomain) -> bool {
    std::thread::scope(|scope| {
        let held = holder.mempool().submit_lock().lock().unwrap();
        let (done, finished) = mpsc::channel();
        scope.spawn(move || {
            let outcome = submitter.receive_tx("test", &submitter.read_chain(), &[0x80]);
            let _ = done.send(outcome.is_err());
        });
        let finished = finished.recv_timeout(Duration::from_secs(2));
        drop(held);
        finished.is_ok()
    })
}

#[test]
fn a_submission_waits_for_its_own_domain() {
    let domain = ToyDomain::new(None, None);

    assert!(!submits_while_held(&domain, &domain));
}

#[test]
fn a_submission_does_not_wait_for_another_domain() {
    let first = ToyDomain::new(None, None);
    let second = ToyDomain::new(None, None);

    assert!(submits_while_held(&first, &second));
}
