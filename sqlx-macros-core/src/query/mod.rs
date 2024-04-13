use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::{fs, io};

use once_cell::sync::Lazy;
use proc_macro2::TokenStream;
use syn::Type;

pub use input::QueryMacroInput;
use quote::{format_ident, quote};
use sqlx_core::database::Database;
use sqlx_core::{column::Column, describe::Describe, type_info::TypeInfo};

use crate::database::DatabaseExt;
use crate::query::data::{hash_string, DynQueryData, QueryData};
use crate::query::input::RecordType;
use either::Either;
use url::Url;
use crate::query::metadata::Metadata;

mod args;
mod config;
mod data;
mod metadata;

mod input;
mod output;

#[derive(Copy, Clone)]
pub struct QueryDriver {
    db_name: &'static str,
    url_schemes: &'static [&'static str],
    expand: fn(QueryMacroInput, QueryDataSource) -> crate::Result<TokenStream>,
}

impl QueryDriver {
    pub const fn new<DB: DatabaseExt>() -> Self
    where
        Describe<DB>: serde::Serialize + serde::de::DeserializeOwned,
    {
        QueryDriver {
            db_name: DB::NAME,
            url_schemes: DB::URL_SCHEMES,
            expand: expand_with::<DB>,
        }
    }
}
pub enum QueryDataSource<'a> {
    Live {
        database_url: &'a str,
        database_url_parsed: Url,
    },
    Cached(DynQueryData),
}

impl<'a> QueryDataSource<'a> {
    pub fn live(database_url: &'a str) -> crate::Result<Self> {
        Ok(QueryDataSource::Live {
            database_url,
            database_url_parsed: database_url.parse()?,
        })
    }

    pub fn matches_driver(&self, driver: &QueryDriver) -> bool {
        match self {
            Self::Live {
                database_url_parsed,
                ..
            } => driver.url_schemes.contains(&database_url_parsed.scheme()),
            Self::Cached(dyn_data) => dyn_data.db_name == driver.db_name,
        }
    }
}

pub fn expand_input<'a>(
    input: QueryMacroInput,
    drivers: impl IntoIterator<Item = &'a QueryDriver>,
) -> crate::Result<TokenStream> {
    let data_source = match Metadata::get()? {
        Metadata {
            offline: false,
            database_url: Some(db_url),
            ..
        } => QueryDataSource::live(db_url)?,

        meta@ Metadata { offline, manifest_dir, .. } => {
            // Try load the cached query metadata file.
            let filename = format!("query-{}.json", hash_string(&input.sql));

            // Check SQLX_OFFLINE_DIR, then local .sqlx, then workspace .sqlx.
            let dirs = [
                || env("SQLX_OFFLINE_DIR").ok().map(PathBuf::from),
                || Some(manifest_dir.join(".sqlx")),
                || Some(meta.workspace_root().expect("failed to find workspace root").join(".sqlx")),
            ];
            let Some(data_file_path) = dirs
                .iter()
                .filter_map(|path| path())
                .map(|path| path.join(&filename))
                .find(|path| path.exists())
            else {
                return Err(
                    if *offline {
                        "`SQLX_OFFLINE=true` but there is no cached data for this query, run `cargo sqlx prepare` (with `sqlx-cli` installed) to update the query cache or unset `SQLX_OFFLINE`".into()
                    } else {
                        format!("set `{}` to use query macros online, or run `cargo sqlx prepare` (with `sqlx-cli` installed) to update the query cache", meta.url_var()).into()
                    }
                );
            };

            QueryDataSource::Cached(DynQueryData::from_data_file(&data_file_path, &input.sql)?)
        }
    };

    for driver in drivers {
        if data_source.matches_driver(&driver) {
            return (driver.expand)(input, data_source);
        }
    }

    match data_source {
        QueryDataSource::Live {
            database_url_parsed,
            ..
        } => Err(format!(
            "no database driver found matching URL scheme {:?}; the corresponding Cargo feature may need to be enabled", 
            database_url_parsed.scheme()
        ).into()),
        QueryDataSource::Cached(data) => {
            Err(format!(
                "found cached data for database {:?} but no matching driver; the corresponding Cargo feature may need to be enabled",
                data.db_name
            ).into())
        }
    }
}

fn expand_with<DB: DatabaseExt>(
    input: QueryMacroInput,
    data_source: QueryDataSource,
) -> crate::Result<TokenStream>
where
    Describe<DB>: DescribeExt,
{
    let (query_data, offline): (QueryData<DB>, bool) = match data_source {
        QueryDataSource::Cached(dyn_data) => (QueryData::from_dyn_data(dyn_data)?, true),
        QueryDataSource::Live { database_url, .. } => {
            let describe = DB::describe_blocking(&input.sql, &database_url)?;
            (QueryData::from_describe(&input.sql, describe), false)
        }
    };

    expand_with_data(input, query_data, offline)
}

// marker trait for `Describe` that lets us conditionally require it to be `Serialize + Deserialize`
trait DescribeExt: serde::Serialize + serde::de::DeserializeOwned {}

impl<DB: Database> DescribeExt for Describe<DB> where
    Describe<DB>: serde::Serialize + serde::de::DeserializeOwned
{
}

fn expand_with_data<DB: DatabaseExt>(
    input: QueryMacroInput,
    data: QueryData<DB>,
    offline: bool,
) -> crate::Result<TokenStream>
where
    Describe<DB>: DescribeExt,
{
    // validate at the minimum that our args match the query's input parameters
    let num_parameters = match data.describe.parameters() {
        Some(Either::Left(params)) => Some(params.len()),
        Some(Either::Right(num)) => Some(num),

        None => None,
    };

    if let Some(num) = num_parameters {
        if num != input.arg_exprs.len() {
            return Err(
                format!("expected {} parameters, got {}", num, input.arg_exprs.len()).into(),
            );
        }
    }

    let args_tokens = args::quote_args(&input, &data.describe)?;

    let query_args = format_ident!("query_args");

    let output = if data
        .describe
        .columns()
        .iter()
        .all(|it| it.type_info().is_void())
    {
        let db_path = DB::db_path();
        let sql = &input.sql;

        quote! {
            ::sqlx::query_with::<#db_path, _>(#sql, #query_args)
        }
    } else {
        match input.record_type {
            RecordType::Generated => {
                let columns = output::columns_to_rust::<DB>(&data.describe)?;

                let record_name: Type = syn::parse_str("Record").unwrap();

                for rust_col in &columns {
                    if rust_col.type_.is_wildcard() {
                        return Err(
                            "wildcard overrides are only allowed with an explicit record type, \
                             e.g. `query_as!()` and its variants"
                                .into(),
                        );
                    }
                }

                let record_fields = columns.iter().map(
                    |&output::RustColumn {
                         ref ident,
                         ref type_,
                         ..
                     }| quote!(#ident: #type_,),
                );

                let mut record_tokens = quote! {
                    #[derive(Debug)]
                    struct #record_name {
                        #(#record_fields)*
                    }
                };

                record_tokens.extend(output::quote_query_as::<DB>(
                    &input,
                    &record_name,
                    &query_args,
                    &columns,
                ));

                record_tokens
            }
            RecordType::Given(ref out_ty) => {
                let columns = output::columns_to_rust::<DB>(&data.describe)?;

                output::quote_query_as::<DB>(&input, out_ty, &query_args, &columns)
            }
            RecordType::Scalar => {
                output::quote_query_scalar::<DB>(&input, &query_args, &data.describe)?
            }
        }
    };

    let ret_tokens = quote! {
        {
            #[allow(clippy::all)]
            {
                use ::sqlx::Arguments as _;

                #args_tokens

                #output
            }
        }
    };

    // Store query metadata only if offline support is enabled but the current build is online.
    // If the build is offline, the cache is our input so it's pointless to also write data for it.
    if !offline {
        // Only save query metadata if SQLX_OFFLINE_DIR is set manually or by `cargo sqlx prepare`.
        // Note: in a cargo workspace this path is relative to the root.
        if let Ok(dir) = env("SQLX_OFFLINE_DIR") {
            let path = PathBuf::from(&dir);

            match fs::metadata(&path) {
                Err(e) => {
                    if e.kind() != io::ErrorKind::NotFound {
                        // Can't obtain information about .sqlx
                        return Err(format!("{e}: {dir}").into());
                    }
                    // .sqlx doesn't exist.
                    return Err(format!("sqlx offline path does not exist: {dir}").into());
                }
                Ok(meta) => {
                    if !meta.is_dir() {
                        return Err(format!(
                            "sqlx offline path exists, but is not a directory: {dir}"
                        )
                        .into());
                    }

                    // .sqlx exists and is a directory, store data.
                    data.save_in(path)?;
                }
            }
        }
    }

    Ok(ret_tokens)
}

/// Get the value of an environment variable, telling the compiler about it if applicable.
fn env(name: &str) -> Result<String, std::env::VarError> {
    #[cfg(procmacro2_semver_exempt)]
    {
        proc_macro::tracked_env::var(name)
    }

    #[cfg(not(procmacro2_semver_exempt))]
    {
        std::env::var(name)
    }
}
