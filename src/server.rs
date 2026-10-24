use std::{collections::HashMap, error::Error, sync::Arc};

use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
        ToolAnnotations,
    },
    service::RequestContext,
};

use crate::{
    config::{Config, ResolvedToolConfig, ToolEffect},
    executor,
    template::{self, ArgumentTemplate},
};

pub(crate) struct CliServer {
    tools: Vec<PreparedTool>,
    index: HashMap<String, usize>,
}

struct PreparedTool {
    config: ResolvedToolConfig,
    templates: Vec<ArgumentTemplate>,
    tool: Tool,
    validator: jsonschema::Validator,
}

impl CliServer {
    pub(crate) fn new(config: Config) -> Result<Self, Box<dyn Error>> {
        let mut tools = Vec::with_capacity(config.tools.len());
        let mut index = HashMap::with_capacity(config.tools.len());

        for resolved in config.tools {
            let name = resolved.expose_name.clone();
            let mut templates = Vec::with_capacity(resolved.args.len());
            for (position, source) in resolved.args.iter().enumerate() {
                let template = ArgumentTemplate::parse(source.clone())
                    .map_err(|error| format!("tool '{name}': args[{position}]: {error}"))?;
                templates.push(template);
            }

            let schema = template::input_schema(&template::parameter_names(&templates));
            let validator = jsonschema::validator_for(&serde_json::Value::Object(schema.clone()))
                .map_err(|error| format!("tool '{name}': invalid input schema: {error}"))?;

            let tool = Tool::new(name.clone(), resolved.description.clone(), Arc::new(schema))
                .with_annotations(annotations_for(resolved.effect));

            index.insert(name, tools.len());
            tools.push(PreparedTool {
                config: resolved,
                templates,
                tool,
                validator,
            });
        }

        Ok(Self { tools, index })
    }
}

impl ServerHandler for CliServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")),
        )
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        if request
            .and_then(|request| request.cursor)
            .is_some_and(|cursor| !cursor.is_empty())
        {
            return Err(ErrorData::invalid_params("Unknown cursor", None));
        }
        Ok(ListToolsResult::with_all_items(
            self.tools.iter().map(|tool| tool.tool.clone()).collect(),
        ))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.index
            .get(name)
            .map(|index| self.tools[*index].tool.clone())
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let Some(prepared) = self
            .index
            .get(request.name.as_ref())
            .map(|index| &self.tools[*index])
        else {
            return Err(ErrorData::invalid_params(
                format!("Unknown tool: {}", request.name),
                None,
            ));
        };

        let arguments = serde_json::Value::Object(request.arguments.unwrap_or_default());
        if let Err(error) = prepared.validator.validate(&arguments) {
            return Ok(CallToolResponse::Complete(CallToolResult::error(vec![
                ContentBlock::text(format!("Invalid arguments: {}", error.masked())),
            ])));
        }
        let arguments = arguments
            .as_object()
            .expect("arguments were constructed as a JSON object");

        match executor::execute(
            &prepared.config,
            &prepared.templates,
            arguments,
            context.ct.cancelled(),
        )
        .await
        {
            Some(result) => Ok(CallToolResponse::Complete(result)),
            None => Err(ErrorData::internal_error("Request cancelled", None)),
        }
    }
}

fn annotations_for(effect: ToolEffect) -> ToolAnnotations {
    match effect {
        ToolEffect::Ro => ToolAnnotations::new().read_only(true).destructive(false),
        ToolEffect::Write => ToolAnnotations::new().read_only(false).destructive(false),
        ToolEffect::Destructive => ToolAnnotations::new().read_only(false).destructive(true),
    }
}
