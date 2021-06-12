use rand::thread_rng;

use super::super::*;

#[test]
fn spendauth_batch_verify() {
    let mut rng = thread_rng();
    let mut batch = batch::Verifier::new();
    for _ in 0..32 {
        let sk = SigningKey::<SpendAuth>::new(&mut rng);
        let vk = VerificationKey::from(&sk);
        let msg = b"BatchVerifyTest";
        let sig = sk.sign(&mut rng, &msg[..]);
        batch.queue((vk.into(), sig, msg));
    }
    assert!(batch.verify(rng).is_ok());
}

#[test]
fn binding_batch_verify() {
    let mut rng = thread_rng();
    let mut batch = batch::Verifier::new();
    for _ in 0..32 {
        let sk = SigningKey::<Binding>::new(&mut rng);
        let vk = VerificationKey::from(&sk);
        let msg = b"BatchVerifyTest";
        let sig = sk.sign(&mut rng, &msg[..]);
        batch.queue((vk.into(), sig, msg));
    }
    assert!(batch.verify(rng).is_ok());
}

#[test]
fn alternating_batch_verify() {
    let mut rng = thread_rng();
    let mut batch = batch::Verifier::new();
    for i in 0..32 {
        let item: batch::Item = match i % 2 {
            0 => {
                let sk = SigningKey::<SpendAuth>::new(&mut rng);
                let vk = VerificationKey::from(&sk);
                let msg = b"BatchVerifyTest";
                let sig = sk.sign(&mut rng, &msg[..]);
                (vk.into(), sig, msg).into()
            }
            1 => {
                let sk = SigningKey::<Binding>::new(&mut rng);
                let vk = VerificationKey::from(&sk);
                let msg = b"BatchVerifyTest";
                let sig = sk.sign(&mut rng, &msg[..]);
                (vk.into(), sig, msg).into()
            }
            _ => unreachable!(),
        };
        batch.queue(item);
    }
    assert!(batch.verify(rng).is_ok());
}

#[test]
fn bad_batch_verify() {
    let mut rng = thread_rng();
    let bad_index = 4; // must be even
    let mut batch = batch::Verifier::new();
    let mut items = Vec::new();
    for i in 0..32 {
        let item: batch::Item = match i % 2 {
            0 => {
                let sk = SigningKey::<SpendAuth>::new(&mut rng);
                let vk = VerificationKey::from(&sk);
                let msg = b"BatchVerifyTest";
                let sig = if i != bad_index {
                    sk.sign(&mut rng, &msg[..])
                } else {
                    sk.sign(&mut rng, b"bad")
                };
                (vk.into(), sig, msg).into()
            }
            1 => {
                let sk = SigningKey::<Binding>::new(&mut rng);
                let vk = VerificationKey::from(&sk);
                let msg = b"BatchVerifyTest";
                let sig = sk.sign(&mut rng, &msg[..]);
                (vk.into(), sig, msg).into()
            }
            _ => unreachable!(),
        };
        items.push(item.clone());
        batch.queue(item);
    }
    assert!(batch.verify(rng).is_err());
    for (i, item) in items.drain(..).enumerate() {
        if i != bad_index {
            assert!(item.verify_single().is_ok());
        } else {
            assert!(item.verify_single().is_err());
        }
    }
}
