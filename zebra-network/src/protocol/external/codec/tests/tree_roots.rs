use super::{compact::frame, *};
use zebra_chain::parallel::tree::TreeRoots;

fn proof(auth: Option<[u8; 32]>) -> TreeRootsProof {
    TreeRootsProof {
        roots: TreeRoots::empty(),
        successor_auth_data_root: auth.map(Into::into),
    }
}

fn gettreeroots_body(count: u32) -> Vec<u8> {
    let mut body = 10u32.to_le_bytes().to_vec();
    body.extend_from_slice(&[7; 32]);
    body.extend_from_slice(&count.to_le_bytes());
    body
}

fn get_tree_roots(count: u32) -> Message {
    Message::GetTreeRoots {
        start_height: block::Height(10),
        final_hash: block::Hash([7; 32]),
        count,
    }
}

#[test]
fn tree_roots_messages_round_trip() {
    let _guard = zebra_test::init();
    let mut codec = Codec::builder()
        .with_max_body_len(MAX_PROTOCOL_MESSAGE_LEN)
        .finish();

    let max_roots =
        vec![proof(Some([7; 32])); usize::try_from(MAX_TREE_ROOTS_PER_REQUEST).unwrap()];
    for message in [
        get_tree_roots(1),
        get_tree_roots(MAX_TREE_ROOTS_PER_REQUEST),
        Message::TreeRoots(None),
        Message::TreeRoots(Some(Vec::new())),
        Message::TreeRoots(Some(vec![proof(None), proof(Some([7; 32]))])),
        Message::TreeRoots(Some(max_roots)),
    ] {
        let mut bytes = BytesMut::new();
        codec.encode(message.clone(), &mut bytes).unwrap();
        assert_eq!(
            &bytes[4..4 + message.command().len()],
            message.command().as_bytes()
        );
        assert!(bytes.len() - HEADER_LEN <= MAX_PROTOCOL_MESSAGE_LEN);
        if let Message::TreeRoots(Some(roots)) = &message {
            if roots.len() == usize::try_from(MAX_TREE_ROOTS_PER_REQUEST).unwrap() {
                assert_eq!(
                    bytes.len() - HEADER_LEN,
                    4 + roots.len() * TreeRootsProof::MAX_SERIALIZED_SIZE
                );
            }
        }
        assert_eq!(codec.decode(&mut bytes).unwrap(), Some(message.clone()));
        assert!(bytes.is_empty());
    }

    let mut bytes = BytesMut::new();
    codec.encode(get_tree_roots(5), &mut bytes).unwrap();
    assert_eq!(&bytes[HEADER_LEN..], gettreeroots_body(5));

    let mut bytes = BytesMut::new();
    codec
        .encode(Message::TreeRoots(Some(vec![proof(None)])), &mut bytes)
        .unwrap();
    let mut body = vec![0, 1];
    TreeRoots::empty().zcash_serialize(&mut body).unwrap();
    body.push(0);
    assert_eq!(&bytes[HEADER_LEN..], body);

    let mut bytes = BytesMut::new();
    codec.encode(Message::TreeRoots(None), &mut bytes).unwrap();
    assert_eq!(&bytes[HEADER_LEN..], [1]);
}

#[test]
fn tree_roots_decoding_rejects_invalid_counts_and_results() {
    let _guard = zebra_test::init();
    let mut codec = Codec::builder()
        .with_max_body_len(MAX_PROTOCOL_MESSAGE_LEN)
        .finish();

    for count in [0, MAX_TREE_ROOTS_PER_REQUEST + 1] {
        assert!(codec
            .decode(&mut frame(b"gettreeroots", &gettreeroots_body(count)))
            .is_err());
    }

    assert!(codec.decode(&mut frame(b"treeroots\0\0\0", &[2])).is_err());

    // Rejected from the count alone, before any entries are read.
    let mut too_many = vec![0, 0xfd];
    too_many.extend_from_slice(
        &u16::try_from(MAX_TREE_ROOTS_PER_REQUEST + 1)
            .unwrap()
            .to_le_bytes(),
    );
    assert!(codec
        .decode(&mut frame(b"treeroots\0\0\0", &too_many))
        .is_err());

    // Presence tags must be canonical; a present digest must have all 32 bytes.
    for suffix in [&[2][..], &[1][..], &[1, 0][..]] {
        let mut body = vec![0, 1];
        TreeRoots::empty().zcash_serialize(&mut body).unwrap();
        body.extend_from_slice(suffix);
        assert!(codec.decode(&mut frame(b"treeroots\0\0\0", &body)).is_err());
    }
}
