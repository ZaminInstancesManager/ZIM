//! The `#[workload_integration]` declaration macro (M16).
//!
//! Attaches to a unit-like integration struct and generates:
//!
//! - a `static` `IntegrationManifest` (a `LazyLock`, built once,
//!   validated at first access), and
//! - the `WorkloadIntegration` impl whose metadata methods read it.
//!
//! The attribute's arguments are validated **at compile time**: an
//! unknown kind, capability, or runtime class is a `compile_error!`,
//! not a runtime panic. The behavioral hooks keep the trait's
//! defaults (`Unsupported`), so an integration grows into its
//! contract by... the trait's defaulted methods are overridden with
//! a hand-written `impl` when the macro is not used; see the SDK's
//! `examples/` for both forms.
//!
//! ```ignore
//! #[workload_integration(
//!     kind = GameServer,
//!     id = "org.zaminmc.workload.minecraft",
//!     version = "0.1.0",
//!     display_name = "Minecraft Server",
//!     description = "Minecraft and Minecraft-compatible game servers.",
//!     required_capabilities = [ConsoleCapable, FilesystemCapable],
//!     optional_capabilities = [Migratable],
//!     runtime_kinds = [Container, Remote],
//! )]
//! pub struct Minecraft;
//! ```
//!
//! The expansion references `::zim_engine_workloads_sdk` and
//! `::zim_engine_domain`, so the applying crate must depend on both.

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{format_ident, quote};
use syn::parse_macro_input;
use syn::punctuated::Punctuated;
use syn::{Ident, ItemStruct, LitStr, Meta, Token};
/// The workload kinds the macro accepts (the domain enum's variants).
const KINDS: &[&str] = &[
    "GameServer",
    "Database",
    "Website",
    "DevProcess",
    "GenericContainer",
];

/// The capabilities the macro accepts (the §4 vocabulary).
const CAPABILITIES: &[&str] = &[
    "ConsoleCapable",
    "ReadyProbeCapable",
    "HealthCheckCapable",
    "FilesystemCapable",
    "NetworkCapable",
    "SnapshotCapable",
    "Migratable",
    "ExecCapable",
    "MetricsCapable",
];

/// The runtime classes the macro accepts.
const RUNTIME_KINDS: &[&str] = &["Container", "Remote", "Host"];

/// Declare a workload integration (see the crate docs).
#[proc_macro_attribute]
pub fn workload_integration(attr: TokenStream, item: TokenStream) -> TokenStream {
    // The attribute's body (the `name = value, …` list between the
    // parentheses — the macro's own path is not part of the stream).
    let arguments = parse_macro_input!(attr with Punctuated::<Meta, Token![,]>::parse_terminated);
    let declaration = parse_macro_input!(item as ItemStruct);
    expand(&arguments, &declaration)
        .unwrap_or_else(|error| error.to_compile_error())
        .into()
}

/// The parsed attribute arguments.
struct Arguments {
    kind: Ident,
    id: LitStr,
    version: LitStr,
    display_name: LitStr,
    description: Option<LitStr>,
    required: Vec<Ident>,
    optional: Vec<Ident>,
    runtime_kinds: Vec<Ident>,
}

/// Parse `name = value` pairs (the attribute's DSL).
fn parse_arguments(nested: &Punctuated<Meta, Token![,]>) -> syn::Result<Arguments> {
    let mut kind = None;
    let mut id = None;
    let mut version = None;
    let mut display_name = None;
    let mut description = None;
    let mut required: Vec<Ident> = Vec::new();
    let mut optional: Vec<Ident> = Vec::new();
    let mut runtime_kinds: Vec<Ident> = Vec::new();

    for meta in nested {
        let syn::Meta::NameValue(pair) = meta else {
            return Err(syn::Error::new_spanned(
                meta,
                "expected `name = value` arguments",
            ));
        };
        let name = pair
            .path
            .get_ident()
            .map(Ident::to_string)
            .unwrap_or_default();
        match name.as_str() {
            "kind" => {
                kind = Some(expect_ident(&pair.value)?);
            }
            "id" => id = Some(expect_string(&pair.value)?),
            "version" => version = Some(expect_string(&pair.value)?),
            "display_name" => display_name = Some(expect_string(&pair.value)?),
            "description" => description = Some(expect_string(&pair.value)?),
            "required_capabilities" => required = expect_idents(&pair.value)?,
            "optional_capabilities" => optional = expect_idents(&pair.value)?,
            "runtime_kinds" => runtime_kinds = expect_idents(&pair.value)?,
            other => {
                return Err(syn::Error::new_spanned(
                    &pair.path,
                    format!(
                        "unknown argument {other:?} (kind, id, version, display_name, \
                         description, required_capabilities, optional_capabilities, \
                         runtime_kinds)"
                    ),
                ));
            }
        }
    }

    let kind = kind.ok_or_else(|| syn::Error::new(Span::call_site(), "kind = … is required"))?;
    let id = id.ok_or_else(|| syn::Error::new(Span::call_site(), "id = … is required"))?;
    let version =
        version.ok_or_else(|| syn::Error::new(Span::call_site(), "version = … is required"))?;
    let display_name = display_name
        .ok_or_else(|| syn::Error::new(Span::call_site(), "display_name = … is required"))?;
    if runtime_kinds.is_empty() {
        return Err(syn::Error::new(
            Span::call_site(),
            "runtime_kinds = […] is required",
        ));
    }
    // Note: an empty `required_capabilities` is legal only for
    // GenericContainer (placement-time negotiation, §5) — the SDK's
    // manifest validation enforces that at first access.
    Ok(Arguments {
        kind,
        id,
        version,
        display_name,
        description,
        required,
        optional,
        runtime_kinds,
    })
}

fn expect_ident(value: &syn::Expr) -> syn::Result<Ident> {
    if let syn::Expr::Path(path) = value {
        if let Some(ident) = path.path.get_ident() {
            return Ok(ident.clone());
        }
    }
    Err(syn::Error::new_spanned(value, "expected a bare identifier"))
}

fn expect_string(value: &syn::Expr) -> syn::Result<LitStr> {
    if let syn::Expr::Lit(literal) = value {
        if let syn::Lit::Str(string) = &literal.lit {
            return Ok(string.clone());
        }
    }
    Err(syn::Error::new_spanned(value, "expected a string literal"))
}

fn expect_idents(value: &syn::Expr) -> syn::Result<Vec<Ident>> {
    let syn::Expr::Array(array) = value else {
        return Err(syn::Error::new_spanned(
            value,
            "expected an identifier list `[A, B]`",
        ));
    };
    let mut idents = Vec::new();
    for element in &array.elems {
        idents.push(expect_ident(element)?);
    }
    Ok(idents)
}

/// Expand the attribute + struct into the manifest static + impl.
fn expand(
    nested: &Punctuated<Meta, Token![,]>,
    declaration: &ItemStruct,
) -> syn::Result<proc_macro2::TokenStream> {
    let arguments = parse_arguments(nested)?;

    // ── Compile-time vocabulary validation ──────────────────────
    let kind_name = arguments.kind.to_string();
    if !KINDS.contains(&kind_name.as_str()) {
        return Err(syn::Error::new(
            arguments.kind.span(),
            format!("unknown workload kind {kind_name:?} ({})", KINDS.join(", ")),
        ));
    }
    for capability in arguments.required.iter().chain(arguments.optional.iter()) {
        let name = capability.to_string();
        if !CAPABILITIES.contains(&name.as_str()) {
            return Err(syn::Error::new(
                capability.span(),
                format!("unknown capability {name:?} ({})", CAPABILITIES.join(", ")),
            ));
        }
    }
    for runtime_kind in &arguments.runtime_kinds {
        let name = runtime_kind.to_string();
        if !RUNTIME_KINDS.contains(&name.as_str()) {
            return Err(syn::Error::new(
                runtime_kind.span(),
                format!(
                    "unknown runtime class {name:?} ({})",
                    RUNTIME_KINDS.join(", ")
                ),
            ));
        }
    }
    // required ∩ optional must be empty.
    for optional in &arguments.optional {
        if arguments.required.iter().any(|r| r == optional) {
            return Err(syn::Error::new(
                optional.span(),
                format!("{optional} is both required and optional"),
            ));
        }
    }

    // ── The generated names ────────────────────────────────────
    let struct_ident = &declaration.ident;
    let manifest_ident =
        format_ident!("__ZIM_MANIFEST_{}", struct_ident.to_string().to_uppercase());
    let kind = &arguments.kind;
    let id = &arguments.id;
    let version = &arguments.version;
    let display_name = &arguments.display_name;
    let description = arguments
        .description
        .clone()
        .unwrap_or_else(|| LitStr::new("", Span::call_site()));
    // Empty lists must emit `CapabilitySet::EMPTY` — an empty
    // `from_iter([...])` cannot infer its element type.
    let required_set = if arguments.required.is_empty() {
        quote! { ::zim_engine_workloads_sdk::CapabilitySet::EMPTY }
    } else {
        let required = &arguments.required;
        quote! {
            ::zim_engine_workloads_sdk::CapabilitySet::from_iter([
                #( ::zim_engine_workloads_sdk::Capability::#required ),*
            ])
        }
    };
    let optional_set = if arguments.optional.is_empty() {
        quote! { ::zim_engine_workloads_sdk::CapabilitySet::EMPTY }
    } else {
        let optional = &arguments.optional;
        quote! {
            ::zim_engine_workloads_sdk::CapabilitySet::from_iter([
                #( ::zim_engine_workloads_sdk::Capability::#optional ),*
            ])
        }
    };
    let runtime_kinds = &arguments.runtime_kinds;

    Ok(quote! {
        // The struct itself, unchanged.
        #declaration

        /// The integration's declaration, validated at first access.
        #[doc(hidden)]
        pub static #manifest_ident: ::std::sync::LazyLock<
            ::zim_engine_workloads_sdk::IntegrationManifest,
        > = ::std::sync::LazyLock::new(|| {
            ::zim_engine_workloads_sdk::IntegrationManifest::builtin(
                ::zim_engine_domain::WorkloadKind::#kind,
                #id,
                #version,
                #display_name,
                #description,
                #required_set,
                #optional_set,
                &[
                    #( ::zim_engine_workloads_sdk::RuntimeKindRef::#runtime_kinds ),*
                ],
            )
        });

        impl ::zim_engine_workloads_sdk::WorkloadIntegration for #struct_ident {
            fn manifest(&self) -> &'static ::zim_engine_workloads_sdk::IntegrationManifest {
                &#manifest_ident
            }
        }
    })
}
