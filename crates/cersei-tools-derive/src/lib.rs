//! cersei-tools-derive: Proc macro for deriving the Tool trait.
//!
//! Usage:
//! ```ignore
//! #[derive(Tool)]
//! #[tool(name = "my_tool", description = "Does something", permission = "read_only")]
//! struct MyTool;
//!
//! #[async_trait]
//! impl ToolExecute for MyTool {
//!     type Input = MyInput;
//!     async fn run(&self, input: MyInput, ctx: &ToolContext) -> ToolResult { ... }
//! }
//! ```

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput};

#[proc_macro_derive(Tool, attributes(tool))]
pub fn derive_tool(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;

    // Parse #[tool(...)] attributes
    let mut tool_name = name.to_string().to_lowercase();
    let mut tool_description = String::new();
    let mut tool_permission = quote! { cersei_tools::PermissionLevel::None };
    let mut tool_category = quote! { cersei_tools::ToolCategory::Custom };

    for attr in &input.attrs {
        if !attr.path().is_ident("tool") {
            continue;
        }
        let _ = attr.parse_nested_meta(|meta| {
            if let Some(ident) = meta.path.get_ident() {
                let key = ident.to_string();
                let value: syn::LitStr = meta.value()?.parse()?;
                let val = value.value();
                match key.as_str() {
                    "name" => tool_name = val,
                    "description" => tool_description = val,
                    "permission" => {
                        tool_permission = match val.as_str() {
                            "none" => quote! { cersei_tools::PermissionLevel::None },
                            "read_only" => quote! { cersei_tools::PermissionLevel::ReadOnly },
                            "write" => quote! { cersei_tools::PermissionLevel::Write },
                            "execute" => quote! { cersei_tools::PermissionLevel::Execute },
                            "dangerous" => quote! { cersei_tools::PermissionLevel::Dangerous },
                            _ => quote! { cersei_tools::PermissionLevel::None },
                        };
                    }
                    "category" => {
                        tool_category = match val.as_str() {
                            "filesystem" => quote! { cersei_tools::ToolCategory::FileSystem },
                            "shell" => quote! { cersei_tools::ToolCategory::Shell },
                            "web" => quote! { cersei_tools::ToolCategory::Web },
                            "memory" => quote! { cersei_tools::ToolCategory::Memory },
                            "orchestration" => quote! { cersei_tools::ToolCategory::Orchestration },
                            "mcp" => quote! { cersei_tools::ToolCategory::Mcp },
                            _ => quote! { cersei_tools::ToolCategory::Custom },
                        };
                    }
                    _ => {}
                }
            }
            Ok(())
        });
    }

    let expanded = quote! {
        #[async_trait::async_trait]
        impl cersei_tools::Tool for #name {
            fn name(&self) -> &str {
                #tool_name
            }

            fn description(&self) -> &str {
                #tool_description
            }

            fn permission_level(&self) -> cersei_tools::PermissionLevel {
                #tool_permission
            }

            fn category(&self) -> cersei_tools::ToolCategory {
                #tool_category
            }

            fn input_schema(&self) -> serde_json::Value {
                let schema = schemars::schema_for!(
                    <Self as cersei_tools::ToolExecute>::Input
                );
                serde_json::to_value(schema).unwrap_or(serde_json::json!({}))
            }

            async fn execute(
                &self,
                input: serde_json::Value,
                ctx: &cersei_tools::ToolContext,
            ) -> cersei_tools::ToolResult {
                match serde_json::from_value::<<Self as cersei_tools::ToolExecute>::Input>(input) {
                    Ok(typed_input) => {
                        <Self as cersei_tools::ToolExecute>::run(self, typed_input, ctx).await
                    }
                    Err(e) => cersei_tools::ToolResult::error(
                        format!("Invalid input for '{}': {}", #tool_name, e)
                    ),
                }
            }
        }
    };

    TokenStream::from(expanded)
}
