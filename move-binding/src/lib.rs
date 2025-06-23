pub mod move_codegen;
mod package_id_resolver;
mod package_provider;
mod types;

#[derive(Copy, Clone)]
pub enum SuiNetwork {
    Mainnet,
    Testnet,
}

impl SuiNetwork {
    pub fn mvr_endpoint(&self) -> &str {
        match self {
            SuiNetwork::Mainnet => "https://mainnet.mvr.mystenlabs.com",
            SuiNetwork::Testnet => "https://testnet.mvr.mystenlabs.com",
        }
    }
    pub fn gql(&self) -> &str {
        match self {
            SuiNetwork::Mainnet => "https://sui-mainnet.mystenlabs.com/graphql",
            SuiNetwork::Testnet => "https://sui-testnet.mystenlabs.com/graphql",
        }
    }
}
