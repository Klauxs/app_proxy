#![cfg(windows)]
use app_proxy_app::configuration::{CatalogPage, catalog_page};
use app_proxy_core::{
    model::{Manifest, ProxySource},
    subscription::{self, saved::SavedNode},
};
use uuid::Uuid;

#[test]
fn catalog_reads_only_metadata_and_supports_all_subscription_protocols() {
    let original: Manifest =
        serde_json::from_str(include_str!("../../../examples/manifest.json")).unwrap();
    let parsed = subscription::parse(include_str!(
        "../../app-proxy-core/tests/fixtures/subscription.yaml"
    ))
    .unwrap();
    for (node, expected) in parsed.nodes.iter().zip([
        "AnyTLS",
        "VLESS",
        "VMess",
        "Shadowsocks",
        "Trojan",
        "Hysteria2",
    ]) {
        let mut manifest: Manifest =
            serde_json::from_str(include_str!("../../../examples/manifest.json")).unwrap();
        let (saved, _) = SavedNode::capture(Uuid::new_v4(), Uuid::new_v4(), node).unwrap();
        let secret_id = saved.secret_id;
        let url_secret_id = Uuid::new_v4();
        manifest.profiles[0].selected_node_id = saved.id;
        manifest.profiles[0].source = ProxySource::Subscription {
            url_secret_id,
            revision: 1,
            nodes: vec![saved],
        };
        let page = catalog_page(manifest, 0, None).unwrap();
        assert_eq!(page.profiles[0].protocol.label(), expected);
        assert!(page.profiles[0].authenticated);
        let json = serde_json::to_string(&page).unwrap();
        for forbidden in [
            secret_id.to_string(),
            url_secret_id.to_string(),
            "fixture%2F,secret".into(),
            "12345678-1234-1234-1234-123456789abc".into(),
        ] {
            assert!(!json.contains(&forbidden));
        }
        let read: CatalogPage = serde_json::from_str(&json).unwrap();
        assert_eq!(read.profiles[0].protocol.label(), expected);
    }
    let old = serde_json::to_value(catalog_page(original, 0, None).unwrap()).unwrap();
    assert_eq!(old["profiles"][0]["protocol"], "http");
}
