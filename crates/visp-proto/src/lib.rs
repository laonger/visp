pub mod visp {
    tonic::include_proto!("visp");
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    #[test]
    fn status_update_default_view_only_is_false() {
        let status = visp::StatusUpdate::default();
        assert!(
            !status.view_only,
            "view_only should default to false in proto3"
        );
    }

    #[test]
    fn status_update_with_view_only_true() {
        let status = visp::StatusUpdate {
            view_only: true,
            ..Default::default()
        };
        let encoded = status.encode_to_vec();
        let decoded = visp::StatusUpdate::decode(encoded.as_slice()).unwrap();
        assert!(
            decoded.view_only,
            "view_only should be true after round-trip"
        );
    }
}
