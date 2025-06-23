use move_binding::move_codegen::MoveCodegen;
use move_binding::SuiNetwork;
use proc_macro::TokenStream;
use proc_macro2::Ident;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{parse_macro_input, DeriveInput, ExprPath, GenericParam, Generics, LitStr, Token};

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
    path: Option<String>,
}

impl Parse for MoveContractArgs {
    fn parse(input: ParseStream) -> Result<Self, syn::Error> {
        let mut alias = None;
        let mut package = None;
        let mut path = None;
        let mut network = SuiNetwork::Mainnet;

        while !input.is_empty() {
            let key: Ident = input.parse()?; // Parse the key (e.g., alias, package, deps)
            input.parse::<Token![=]>()?; // Expect '=' token

            if key == "alias" {
                alias = Some(input.parse::<LitStr>()?.value()); // Parse string literal
            } else if key == "package" {
                package = Some(input.parse::<LitStr>()?.value()); // Parse string literal
            } else if key == "base_path" {
                let p = input.parse::<ExprPath>()?.path;
                path = Some(quote!(#p).to_string()); // Parse string literal
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
            path,
        })
    }
}

#[proc_macro]
pub fn move_contract(input: TokenStream) -> TokenStream {
    let MoveContractArgs {
        network,
        package_alias,
        package,
        path,
    } = parse_macro_input!(input as MoveContractArgs);
    MoveCodegen::expand(
        network,
        &package,
        &package_alias,
        &path.unwrap_or("crate".to_string()),
    )
    .unwrap()
    .into()
}
