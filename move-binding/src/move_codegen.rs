use crate::package_provider::{ModuleProvider, MoveModuleProvider};
use crate::types::ToRustType;
use crate::SuiNetwork;
use anyhow::anyhow;
use itertools::Itertools;
use move_binary_format::normalized::{Enum, Function, Struct, Type};
use move_core_types::account_address::AccountAddress;
use move_core_types::identifier::Identifier;
use once_cell::sync::Lazy;
use proc_macro2::{Ident, TokenStream};
use quote::quote;
use std::collections::{BTreeMap, HashMap};
use std::sync::RwLock;

pub static BINDING_REGISTRY: Lazy<RwLock<HashMap<AccountAddress, String>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

pub struct MoveCodegen;

impl MoveCodegen {
    pub fn expand(
        network: SuiNetwork,
        package: &str,
        package_alias: &str,
        base_path: &str,
    ) -> Result<TokenStream, anyhow::Error> {
        let module_provider = MoveModuleProvider::new(network);
        let package = module_provider.get_package(package)?;

        // register package path
        let mut cache = BINDING_REGISTRY
            .write()
            .map_err(|e| anyhow!("Failed to acquire write lock: {}", e))?;
        package
            .type_origin_table
            .iter()
            .flat_map(|(_, m)| m.values())
            .dedup()
            .for_each(|addr| {
                cache.insert(addr.clone(), format!("{base_path}::{package_alias}"));
            });
        drop(cache);

        let module_tokens = package
            .module_map
            .iter()
            .map(|(module_name, module)| {
                let module_ident = Ident::new(module_name, proc_macro2::Span::call_site());
                let type_origin_table = package
                    .type_origin_table
                    .get(module_name)
                    .cloned()
                    .unwrap_or_default();

                let mut struct_fun_tokens =
                    Self::create_structs(&module.structs, &type_origin_table)?;
                struct_fun_tokens.extend(Self::create_enums(&module.enums, &type_origin_table));
                struct_fun_tokens.extend(Self::create_funs(&module.functions));

                Ok::<_, anyhow::Error>(if struct_fun_tokens.is_empty() {
                    quote! {}
                } else {
                    let addr_byte_ident = module.address.to_vec();
                    quote! {
                        pub mod #module_ident{
                            use std::str::FromStr;
                            use move_binding_derive::{MoveStruct, Key};
                            use move_types::{MoveType, Address, Identifier, ObjectId};
                            use move_types::functions::{Arg, Ref, MutRef};
                            pub const PACKAGE_ID: Address = Address::new([#(#addr_byte_ident),*]);
                            pub const MODULE_NAME: &str = #module_name;
                            #(#struct_fun_tokens)*
                        }
                    }
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        let package_ident = Ident::new(&package_alias, proc_macro2::Span::call_site());
        let version = package.version;

        Ok(quote! {
            pub mod #package_ident{
                pub const PACKAGE_VERSION:u64 = #version;
                #(#module_tokens)*
            }
        })
    }

    fn create_structs(
        structs: &BTreeMap<Identifier, Struct>,
        type_origin_ids: &HashMap<String, AccountAddress>,
    ) -> Result<Vec<TokenStream>, anyhow::Error> {
        structs
            .iter()
            .map(|(name, move_struct)| {
                Self::create_struct(name.as_str(), move_struct, type_origin_ids)
            })
            .collect()
    }

    fn create_struct(
        struct_name: &str,
        move_struct: &Struct,
        type_origin_id: &HashMap<String, AccountAddress>,
    ) -> Result<TokenStream, anyhow::Error> {
        let (type_parameters, phantoms) = move_struct.type_parameters.iter().enumerate().fold(
            (vec![], vec![]),
            |(mut type_parameters, mut phantoms), (i, v)| {
                let ident = Ident::new(&format!("T{i}"), proc_macro2::Span::call_site());
                type_parameters.push(quote! {#ident});

                if v.is_phantom {
                    let name =
                        Ident::new(&format!("phantom_data_{i}"), proc_macro2::Span::call_site());
                    let type_: syn::Type =
                        syn::parse_str(&format!("std::marker::PhantomData<T{i}>")).unwrap();
                    phantoms.push(quote! {#name: #type_,})
                }
                (type_parameters, phantoms)
            },
        );

        let struct_ident = Ident::new(&struct_name.to_string(), proc_macro2::Span::call_site());
        let field_tokens = move_struct
            .fields
            .iter()
            .map(|field| {
                let field_ident = Ident::new(
                    &escape_keyword(field.name.as_str()),
                    proc_macro2::Span::call_site(),
                );
                let field_type: syn::Type = syn::parse_str(&field.type_.to_rust_type())?;
                Ok(quote! {pub #field_ident: #field_type,})
            })
            .collect::<Result<Vec<_>, anyhow::Error>>()?;

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
        Ok(if type_parameters.is_empty() {
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
        })
    }

    fn create_enums(
        enums: &BTreeMap<Identifier, Enum>,
        type_origin_ids: &HashMap<String, AccountAddress>,
    ) -> Vec<TokenStream> {
        enums
            .iter()
            .map(|(name, move_enum)| Self::create_enum(name.as_str(), move_enum, type_origin_ids))
            .collect()
    }

    fn create_enum(
        enum_name: &str,
        move_enum: &Enum,
        type_origin_id: &HashMap<String, AccountAddress>,
    ) -> TokenStream {
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
                    let field_type: syn::Type =
                        syn::parse_str(&field.type_.to_rust_type()).unwrap();
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

    fn create_funs(funs: &BTreeMap<Identifier, Function>) -> Vec<TokenStream> {
        funs.iter()
            .flat_map(|(name, fun)| Self::create_fun(name.as_str(), fun))
            .collect()
    }

    fn create_fun(fun_name: &str, fun: &Function) -> Option<TokenStream> {
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
}

fn escape_keyword(name: &str) -> String {
    match name {
        "for" | "ref" => {
            format!("{name}_")
        }
        _ => name.to_string(),
    }
}
