use insta::assert_snapshot;
use move_binding::move_codegen::MoveCodegen;
use move_binding::SuiNetwork;
use syn::parse2;

#[test]
fn test_generate_sui_packages() {
    test_package(SuiNetwork::Mainnet, "0x1", "move_lib");
    test_package(SuiNetwork::Mainnet, "0x2", "sui");
    test_package(SuiNetwork::Mainnet, "0x3", "sui_system");
    test_package(SuiNetwork::Mainnet, "0xb", "bridge");
    test_package(
        SuiNetwork::Testnet,
        "0x8270feb7375eee355e64fdb69c50abb6b5f9393a722883c1cf45f8e26048810a",
        "wal",
    );
    test_package(
        SuiNetwork::Testnet,
        "0xd84704c17fc870b8764832c535aa6b11f21a95cd6f5bb38a9b07d2cf42220c66",
        "walrus",
    );
}

fn test_package(network: SuiNetwork, package: &str, alias: &str) {
    let ts = MoveCodegen::expand(network, package, alias, "crate").unwrap();
    let file = parse2::<syn::File>(ts.clone()).expect("Failed to parse TokenStream");
    let pretty = prettyplease::unparse(&file);
    assert_snapshot!(package, pretty)
}
