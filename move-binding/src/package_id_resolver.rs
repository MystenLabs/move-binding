use crate::SuiNetwork;
use serde_json::Value;
use std::str::FromStr;
use sui_sdk_types::Address;

pub struct PackageIdResolver;

impl PackageIdResolver {
    pub fn resolve_package_id(
        network: SuiNetwork,
        package: &str,
    ) -> Result<Address, anyhow::Error> {
        Ok(if package.contains("@") || package.contains(".sui") {
            Self::resolve_mvr_name(package, network.mvr_endpoint())?
        } else {
            Address::from_str(&package)?
        })
    }

    fn resolve_mvr_name(package: &str, url: &str) -> Result<Address, anyhow::Error> {
        let client = reqwest::blocking::Client::new();
        let name = client
            .get(format!("{url}/v1/resolution/{package}"))
            .send()?;
        Ok(serde_json::from_value(
            name.json::<Value>()?["package_id"].clone(),
        )?)
    }
}
