mod package_provider;

use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;
use std::sync::LazyLock;

use dashmap::DashMap;
use itertools::Itertools;
use move_binary_format::normalized::{Enum, Function, Struct, Type};
use move_core_types::account_address::AccountAddress;
use move_core_types::identifier::Identifier;
use proc_macro::TokenStream;
use proc_macro2::Ident;
use quote::{quote, ToTokens};
use serde_json::Value;

use crate::package_provider::{ModuleProvider, MoveModuleProvider};

use sui_sdk_types::Address;
use syn::parse::{Parse, ParseStream};
use syn::{parse_macro_input, DeriveInput, ExprArray, GenericParam, Generics, LitStr, Path, Token};

#[proc_macro_derive(Key)]
pub fn key_derive(input: TokenStream) -> TokenStream {
    let ast: DeriveInput = syn::parse(input).unwrap();
    let name = &ast.ident;

    let types = extract_type_ident(&ast.generics);

    let (types_with_trait, types) = if types.is_empty() {
        (quote! {}, quote! {})
    } else {
        (
            quote! {<#(#types:move_types::MoveType),*>},
            quote! {<#(#types),*>},
        )
    };

    let gen = quote! {
        impl #types_with_trait move_types::Key for #name #types {
            fn id(&self) -> &move_types::ObjectId {
                &self.id
            }
        }
    };
    gen.into()
}

fn extract_type_ident(generics: &Generics) -> Vec<Ident> {
    generics
        .params
        .iter()
        .flat_map(|p| {
            if let GenericParam::Type(t) = p {
                Some(t.ident.clone())
            } else {
                None
            }
        })
        .collect()
}

#[proc_macro_derive(MoveStruct)]
pub fn move_struct_derive(input: TokenStream) -> TokenStream {
    let ast: DeriveInput = syn::parse(input).unwrap();
    let name = &ast.ident;

    let types = extract_type_ident(&ast.generics);
    let name_str = name.to_string();

    let gen = if types.is_empty() {
        quote! {
            impl move_types::MoveStruct for #name {
                fn struct_type() -> move_types::StructTag {
                    move_types::StructTag {
                        address: Self::TYPE_ORIGIN_ID,
                        module: move_types::Identifier::from_str(MODULE_NAME).unwrap(),
                        name: move_types::Identifier::from_str(#name_str).unwrap(),
                        type_params: vec![],
                    }
                }
            }
        }
    } else {
        quote! {
            impl <#(#types:move_types::MoveType), *> move_types::MoveStruct for #name<#(#types),*> {
                fn struct_type() -> move_types::StructTag {
                    move_types::StructTag {
                        address: Self::TYPE_ORIGIN_ID,
                        module: move_types::Identifier::from_str(MODULE_NAME).unwrap(),
                        name: move_types::Identifier::from_str(#name_str).unwrap(),
                        type_params: vec![#(#types::type_()),*],
                    }
                }
            }
        }
    };
    gen.into()
}

struct MoveContractArgs {
    network: SuiNetwork,
    package_alias: String,
    package: String,
    deps: Vec<Path>,
}

impl Parse for MoveContractArgs {
    fn parse(input: ParseStream) -> Result<Self, syn::Error> {
        let mut alias = None;
        let mut package = None;
        let mut deps = Vec::new();
        let mut network = SuiNetwork::Mainnet;

        while !input.is_empty() {
            let key: Ident = input.parse()?; // Parse the key (e.g., alias, package, deps)
            input.parse::<Token![=]>()?; // Expect '=' token

            if key == "alias" {
                alias = Some(input.parse::<LitStr>()?.value()); // Parse string literal
            } else if key == "package" {
                package = Some(input.parse::<LitStr>()?.value()); // Parse string literal
            } else if key == "deps" {
                let array: ExprArray = input.parse()?; // Parse `[ sui, move_stdlib ]`
                deps = array
                    .elems
                    .iter()
                    .map(|expr| match expr {
                        syn::Expr::Path(path) => path.path.clone(),
                        _ => panic!("Expected an identifier in deps list"),
                    })
                    .collect();
            } else if key == "network" {
                if let Ok(lit) = input.parse::<LitStr>() {
                    network = match lit.value().to_lowercase().as_str() {
                        "mainnet" => SuiNetwork::Mainnet,
                        "testnet" => SuiNetwork::Testnet,
                        _ => {
                            return Err(syn::Error::new(
                                key.span(),
                                "Unknown network, only ['mainnet', 'testnet'] are supported.",
                            ))
                        }
                    };
                }
            } else {
                return Err(syn::Error::new(key.span(), "Unknown key"));
            }

            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?; // Consume optional comma
            }
        }

        Ok(MoveContractArgs {
            network,
            package_alias: alias.ok_or_else(|| syn::Error::new(input.span(), "Missing alias"))?,
            package: package.ok_or_else(|| syn::Error::new(input.span(), "Missing package"))?,
            deps,
        })
    }
}

#[proc_macro]
pub fn move_contract(input: TokenStream) -> TokenStream {
    let MoveContractArgs {
        network,
        package_alias,
        package,
        deps,
    } = parse_macro_input!(input as MoveContractArgs);

    let package_id = if package.contains("@") || package.contains(".sui") {
        resolve_mvr_name(package, &network.mvr_endpoint()).expect("Cannot resolve mvr name")
    } else {
        Address::from_str(&package).expect("Error parsing package id.")
    };

    let module_provider = MoveModuleProvider::new(network);
    let package = module_provider.get_package(package_id);

    let package_address = package
        .module_map
        .values()
        .next()
        .map(|module| module.address)
        .expect("None package address");

    for dep in &deps {
        let dep_path_str = dep
            .to_token_stream()
            .to_string()
            .split_whitespace()
            .collect::<String>();

        let dep_name = dep_path_str.split("::").last().unwrap_or_default().trim();

        for mut alias_entry in PACKAGE_ALIAS_MAP.iter_mut() {
            if *alias_entry.value() == dep_name {
                *alias_entry.value_mut() = dep_path_str.clone();
            }
        }
    }

    let module_tokens = package.module_map.iter().map(|(module_name, module)| {
        let module_ident = Ident::new(module_name, proc_macro2::Span::call_site());

        let type_origin_table = package
            .type_origin_table
            .get(module_name)
            .cloned()
            .unwrap_or_default();
        let mut struct_fun_tokens = create_structs(&module.structs, &type_origin_table);

        if !module.enums.is_empty() {
            let enum_tokens = create_enums(&module.enums, &type_origin_table);
            struct_fun_tokens.extend(enum_tokens);
        }

        if !module.functions.is_empty() {
            let fun_impl = create_funs(&module.functions);
            struct_fun_tokens.extend(fun_impl);
        }

        if struct_fun_tokens.is_empty() {
            quote! {}
        } else {
            let addr_byte_ident = module.address.to_vec();
            quote! {
                pub mod #module_ident{
                    use super::*;
                    pub const PACKAGE_ID: Address = Address::new([#(#addr_byte_ident),*]);
                    pub const MODULE_NAME: &str = #module_name;
                    #(#struct_fun_tokens)*
                }
            }
        }
    });

    let package_ident = Ident::new(&package_alias, proc_macro2::Span::call_site());
    let version = package.version;
    let expanded = quote! {
        pub mod #package_ident{
            // #(use #deps;)*
            use std::str::FromStr;
            use move_binding_derive::{Key, MoveStruct};
            use move_types::{MoveType, Address, Identifier, TypeTag, StructTag};
            use move_types::functions::{Arg, Ref, MutRef};
            pub const PACKAGE_VERSION:u64 = #version;
            #(#module_tokens)*
        }
    };

    // Defer registering the current package in the alias map until after its Rust bindings are fully generated.
    //
    // Registering it too early would cause submodules or dependencies to incorrectly resolve to the current
    // (still incomplete) package path. For example, code might expand to `crate::current_package::submodule`
    // before `submodule` actually exists, leading to Rust compilation errors like:
    // "failed to resolve: use of unresolved module or unlinked crate".
    register_alias_mapping(package_address.into(), &package_alias);
    expanded.into()
}

static PACKAGE_ALIAS_MAP: LazyLock<DashMap<AccountAddress, String>> =
    LazyLock::new(|| DashMap::new());

fn register_alias_mapping(address: AccountAddress, alias: &str) {
    PACKAGE_ALIAS_MAP.insert(address, alias.to_string());
}

fn get_alias_for_address(address: &AccountAddress) -> Option<String> {
    PACKAGE_ALIAS_MAP.get(address).map(|r| r.value().clone())
}

fn resolve_mvr_name(package: String, url: &str) -> Option<Address> {
    let client = reqwest::blocking::Client::new();
    let name = client
        .get(format!("{url}/v1/resolution/{package}"))
        .send()
        .ok()?;

    serde_json::from_value(name.json::<Value>().ok()?["package_id"].clone()).ok()
}

fn create_structs(
    structs: &BTreeMap<Identifier, Struct>,
    type_origin_ids: &HashMap<String, AccountAddress>,
) -> Vec<proc_macro2::TokenStream> {
    structs
        .iter()
        .map(|(name, move_struct)| create_struct(name.as_str(), move_struct, type_origin_ids))
        .collect()
}

fn create_struct(
    struct_name: &str,
    move_struct: &Struct,
    type_origin_id: &HashMap<String, AccountAddress>,
) -> proc_macro2::TokenStream {
    let (type_parameters, phantoms) = move_struct.type_parameters.iter().enumerate().fold(
        (vec![], vec![]),
        |(mut type_parameters, mut phantoms), (i, v)| {
            let ident = Ident::new(&format!("T{i}"), proc_macro2::Span::call_site());
            type_parameters.push(quote! {#ident});

            if v.is_phantom {
                let name = Ident::new(&format!("phantom_data_{i}"), proc_macro2::Span::call_site());
                let type_: syn::Type =
                    syn::parse_str(&format!("std::marker::PhantomData<T{i}>")).unwrap();
                phantoms.push(quote! {#name: #type_,})
            }
            (type_parameters, phantoms)
        },
    );

    let struct_ident = Ident::new(&struct_name.to_string(), proc_macro2::Span::call_site());
    let field_tokens = move_struct.fields.iter().map(|field| {
        let field_ident = Ident::new(
            &escape_keyword(field.name.as_str()),
            proc_macro2::Span::call_site(),
        );
        let field_type: syn::Type = syn::parse_str(&field.type_.to_rust_type()).unwrap();
        quote! {pub #field_ident: #field_type,}
    });

    let mut derives = vec![
        quote! {serde::Deserialize},
        quote! {serde::Serialize},
        quote! {Debug},
        quote! {MoveStruct},
    ];

    if move_struct.abilities.has_key() {
        derives.push(quote! {Key});
    }

    let addr_byte_ident = type_origin_id[struct_name].to_vec();
    if type_parameters.is_empty() {
        quote! {
            #[derive(#(#derives),*)]
            pub struct #struct_ident {
                #(#field_tokens)*
            }
            impl #struct_ident{
                pub const TYPE_ORIGIN_ID: Address = Address::new([#(#addr_byte_ident),*]);
            }
        }
    } else {
        quote! {
            #[derive(#(#derives),*)]
            pub struct #struct_ident<#(#type_parameters),*> {
                #(#field_tokens)*
                #(#phantoms)*
            }
            impl <#(#type_parameters),*> #struct_ident<#(#type_parameters),*>{
                pub const TYPE_ORIGIN_ID: Address = Address::new([#(#addr_byte_ident),*]);
            }
        }
    }
}

fn create_enums(
    enums: &BTreeMap<Identifier, Enum>,
    type_origin_ids: &HashMap<String, AccountAddress>,
) -> Vec<proc_macro2::TokenStream> {
    enums
        .iter()
        .map(|(name, move_enum)| create_enum(name.as_str(), move_enum, type_origin_ids))
        .collect()
}

fn create_enum(
    enum_name: &str,
    move_enum: &Enum,
    type_origin_id: &HashMap<String, AccountAddress>,
) -> proc_macro2::TokenStream {
    let enum_ident = Ident::new(&enum_name.to_string(), proc_macro2::Span::call_site());
    let variant_tokens = move_enum.variants.iter().map(|variant| {
        let variant_ident = Ident::new(
            &escape_keyword(variant.name.as_str()),
            proc_macro2::Span::call_site(),
        );

        if variant.fields.is_empty() {
            return quote! {#variant_ident,};
        }

        if variant
            .fields
            .iter()
            .enumerate()
            .all(|(i, field)| field.name.to_string() == format!("pos{}", i))
        {
            let field_types = variant.fields.iter().map(|field| {
                let field_type: syn::Type = syn::parse_str(&field.type_.to_rust_type()).unwrap();
                quote! {#field_type,}
            });

            return quote! {
                #variant_ident(#(#field_types)*),
            };
        }

        let field_tokens = variant.fields.iter().map(|field| {
            let field_ident = Ident::new(
                &escape_keyword(field.name.as_str()),
                proc_macro2::Span::call_site(),
            );
            let field_type: syn::Type = syn::parse_str(&field.type_.to_rust_type()).unwrap();
            quote! {#field_ident: #field_type,}
        });
        quote! { #variant_ident {#(#field_tokens)*},}
    });

    let derives = vec![
        quote! {serde::Deserialize},
        quote! {serde::Serialize},
        quote! {Debug},
        quote! {MoveStruct},
    ];

    let addr_byte_ident = type_origin_id[enum_name].to_vec();

    quote! {
        #[derive(#(#derives),*)]
        pub enum #enum_ident{
            #(#variant_tokens)*
        }

        impl #enum_ident{
            pub const TYPE_ORIGIN_ID: Address = Address::new([#(#addr_byte_ident),*]);
        }
    }
}

fn create_funs(funs: &BTreeMap<Identifier, Function>) -> Vec<proc_macro2::TokenStream> {
    funs.iter()
        .flat_map(|(name, fun)| create_fun(name.as_str(), fun))
        .collect()
}

fn create_fun(fun_name: &str, fun: &Function) -> Option<proc_macro2::TokenStream> {
    let (param_names, mut params, need_lifetime) = fun.parameters
        .iter()
        .enumerate()
        .fold((vec![], vec![], false), |(mut param_names, mut params, mut lifetime), (i, move_type)| {
            let field_ident = Ident::new(&format!("p{i}"), proc_macro2::Span::call_site());
            lifetime = lifetime || move_type.is_ref();
            match &move_type {
                Type::Reference(r) |
                Type::MutableReference(r) => {
                    // filter out TxContext
                    if matches!(&**r, Type::Struct{address, name, ..} if address == &AccountAddress::TWO && name.as_str() == "TxContext") {
                        return (param_names, params, lifetime);
                    }
                }
                _ => {}
            }
            param_names.push(quote! {#field_ident});
            let field_type: syn::Type = syn::parse_str(&move_type.to_arg_type()).unwrap();
            params.push(quote! {#field_ident: #field_type});
            (param_names, params, lifetime)
        });
    params.insert(
        0,
        quote! {builder: &mut sui_transaction_builder::TransactionBuilder},
    );

    let returns = fun
        .return_
        .iter()
        .flat_map(|move_type| syn::parse_str::<syn::Type>(&move_type.to_arg_type()).ok())
        .collect::<Vec<_>>();

    let (types, mut types_with_ability) = fun.type_parameters.iter().enumerate().fold(
        (vec![], vec![]),
        |(mut types, mut types_with_ability), (i, v)| {
            let ident = Ident::new(&format!("T{i}"), proc_macro2::Span::call_site());
            types.push(ident.clone());
            let mut abilities = vec![];
            if v.has_key() {
                abilities.push(quote! {move_types::Key});
            } else {
                abilities.push(quote! {MoveType});
            }
            types_with_ability.push(quote! {#ident: #(#abilities)+*});
            (types, types_with_ability)
        },
    );

    if need_lifetime || fun.return_.iter().any(|t| t.is_ref()) {
        types_with_ability.insert(0, quote! {'a})
    }

    let fun_ident = Ident::new(fun_name, proc_macro2::Span::call_site());

    let (maybe_returns, maybe_into) = if let Some(t) = returns.first() {
        (quote! { -> #t}, quote! {.into()})
    } else {
        (quote! {}, quote! {;})
    };

    let sig = if types_with_ability.is_empty() {
        quote! {
            pub fn #fun_ident(#(#params),*) #maybe_returns
        }
    } else {
        quote! {
            pub fn #fun_ident <#(#types_with_ability),*>(#(#params),*) #maybe_returns
        }
    };

    let fun_impl = quote! {
        #sig {
            #(let #param_names = #param_names.resolve_arg(builder);)*
            builder.move_call(
                sui_transaction_builder::Function::new(
                    PACKAGE_ID,
                    Identifier::from_str(MODULE_NAME).unwrap(),
                    Identifier::from_str(#fun_name).unwrap(),
                    vec![#(#types::type_()),*],
                ),
                vec![#(#param_names.into()),*],
            )
            #maybe_into
        }
    };
    Some(fun_impl)
}

enum SuiNetwork {
    Mainnet,
    Testnet,
}

impl SuiNetwork {
    fn mvr_endpoint(&self) -> &str {
        match self {
            SuiNetwork::Mainnet => "https://mainnet.mvr.mystenlabs.com",
            SuiNetwork::Testnet => "https://testnet.mvr.mystenlabs.com",
        }
    }
    fn gql(&self) -> &str {
        match self {
            SuiNetwork::Mainnet => "https://sui-mainnet.mystenlabs.com/graphql",
            SuiNetwork::Testnet => "https://sui-testnet.mystenlabs.com/graphql",
        }
    }
}

fn escape_keyword(name: &str) -> String {
    match name {
        "for" | "ref" => {
            format!("{name}_")
        }
        _ => name.to_string(),
    }
}

trait ToRustType {
    fn to_rust_type(&self) -> String;
    fn is_ref(&self) -> bool;
    fn to_arg_type(&self) -> String;
    fn try_resolve_known_types(&self) -> String;
}

impl ToRustType for Type {
    fn to_rust_type(&self) -> String {
        match self {
            Self::Bool => "bool".to_string(),
            Self::U8 => "u8".to_string(),
            Self::U16 => "u16".to_string(),
            Self::U32 => "u32".to_string(),
            Self::U64 => "u64".to_string(),
            Self::U128 => "u128".to_string(),
            Self::U256 => "move_types::U256".to_string(),
            Self::Address => "Address".to_string(),
            Self::Signer => "Address".to_string(),
            t @ Self::Struct { .. } => t.try_resolve_known_types(),
            Self::Vector(t) => {
                format!("Vec<{}>", t.to_rust_type())
            }
            Self::Reference(t) => {
                format!("&'static {}", t.to_rust_type())
            }
            Self::MutableReference(t) => {
                format!("&'static mut {}", t.to_rust_type())
            }
            Self::TypeParameter(index) => format!("T{index}"),
        }
    }

    fn is_ref(&self) -> bool {
        match self {
            Self::Reference(_) | Self::MutableReference(_) => true,
            _ => false,
        }
    }

    fn to_arg_type(&self) -> String {
        match self {
            Self::Reference(t) => {
                format!("Ref<'a, {}>", t.to_rust_type())
            }
            Self::MutableReference(t) => {
                format!("MutRef<'a, {}>", t.to_rust_type())
            }
            _ => format!("Arg<{}>", self.to_rust_type()),
        }
    }

    fn try_resolve_known_types(&self) -> String {
        if let Self::Struct {
            address,
            module,
            name,
            type_arguments,
        } = self
        {
            match (address, module.as_str(), name.as_str()) {
                (&AccountAddress::ONE, "type_name", "TypeName") => "String".to_string(),
                (&AccountAddress::ONE, "string", "String") => "String".to_string(),
                (&AccountAddress::ONE, "ascii", "String") => "String".to_string(),
                (&AccountAddress::ONE, "option", "Option") => {
                    format!("Option<{}>", type_arguments[0].to_rust_type())
                }

                (&AccountAddress::TWO, "object", "UID") => "sui_sdk_types::ObjectId".to_string(),
                (&AccountAddress::TWO, "object", "ID") => "sui_sdk_types::ObjectId".to_string(),
                _ => {
                    let base_path = if let Some(alias) = get_alias_for_address(address) {
                        format!("{alias}::{module}")
                    } else {
                        module.to_string()
                    };

                    let type_args = if type_arguments.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "<{}>",
                            type_arguments.iter().map(|ty| ty.to_rust_type()).join(", ")
                        )
                    };

                    format!("{base_path}::{name}{type_args}")
                }
            }
        } else {
            unreachable!()
        }
    }
}
