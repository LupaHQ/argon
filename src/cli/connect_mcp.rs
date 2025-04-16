use anyhow::Result;
use clap::Parser;
use log::info;
use rmcp::{
	model::{
		CallToolResult, Content, GetPromptRequestParam, GetPromptResult, Implementation, ListPromptsResult,
		ListResourceTemplatesResult, ListResourcesResult, LoggingLevel, LoggingMessageNotification,
		LoggingMessageNotificationMethod, LoggingMessageNotificationParam, Notification, PaginatedRequestParam,
		ProtocolVersion, ReadResourceRequestParam, ReadResourceResult, ServerCapabilities, ServerInfo,
		ServerNotification,
	},
	service::{RequestContext, RoleServer},
	tool,
	transport::io::stdio,
	Error as McpError, Peer, ServerHandler, ServiceExt,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use std::{env, sync::Arc};
use tokio::sync::Mutex;

/// Connect to an MCP endpoint (used internally by Cursor)
#[derive(Parser)]
pub struct ConnectMcp {
	/// Whether to run in client mode (Currently ignored, always runs as stdio server)
	#[arg(long)]
	client: bool,
}

// Define argument struct for our tools
#[derive(Debug, Deserialize, JsonSchema)]
struct PineconeQueryArgs {
	#[schemars(description = "The query text to send to the Pinecone assistant.")]
	query: String,
}

// Main server struct (Removed host and port)
#[derive(Clone)]
struct ArgonMcpServer {
	peer: Arc<Mutex<Option<Peer<RoleServer>>>>,
	// host: String, // Removed
	// port: u16,    // Removed
}

impl ArgonMcpServer {
	// Updated new function (Removed host and port parameters)
	fn new() -> Self {
		// The peer field is initialized as None here, but it will be properly set during
		// the server.serve(stdio()) call via the ServiceExt::serve method from the rmcp crate.
		// The serve method sets up the transport, performs initialization handshake, and
		// populates the peer field with the connected Peer object that handles communication.
		Self {
			peer: Arc::new(Mutex::new(None)),
			// host, // Removed
			// port, // Removed
		}
	}

	// Helper function to send log messages (matches RustDocsServer approach)
	fn send_log(&self, level: LoggingLevel, message: &str) {
		// Log locally first
		match level {
			LoggingLevel::Info => info!("{:?}: {}", level, message), // Use debug format for enum
			_ => info!("{:?}: {}", level, message),
		}

		// Clone the Arc pointer to the mutex-protected peer field to move it into
		// the async task. The peer field may be None early in the server lifecycle
		// before the serve() method completes initialization. The if-let inside the
		// task safely handles this case, only attempting to send notifications when
		// peer is available.
		let peer_arc = Arc::clone(&self.peer);
		let message_string = message.to_string(); // Clone message for the async task

		tokio::spawn(async move {
			let mut peer_guard = peer_arc.lock().await;
			if let Some(peer) = peer_guard.as_mut() {
				let params = LoggingMessageNotificationParam {
					level,
					logger: None, // Or some logger name if desired
					data: serde_json::Value::String(message_string),
				};
				let log_notification: LoggingMessageNotification = Notification {
					method: LoggingMessageNotificationMethod,
					params,
				};
				let server_notification = ServerNotification::LoggingMessageNotification(log_notification);
				if let Err(e) = peer.send_notification(server_notification).await {
					// Log error locally if sending notification fails
					info!("Failed to send MCP log notification: {}", e);
				}
			}
		});
	}

	// --- Internal Helper for Pinecone API Call ---
	async fn call_pinecone_assistant(&self, assistant_name: &str, query: &str, top_k: u32) -> Result<String, McpError> {
		let api_key = env::var("PINECONE_API_KEY")
			.map_err(|_| McpError::internal_error("PINECONE_API_KEY environment variable not set", None))?;

		let endpoint = format!(
			"https://prod-1-data.ke.pinecone.io/assistant/chat/{}/context",
			assistant_name
		);

		let client = reqwest::Client::new();
		let payload = json!({
			"query": query,
			"top_k": top_k
		});

		let response = client
			.post(&endpoint)
			.header("Api-Key", api_key)
			.header("Accept", "application/json")
			.header("Content-Type", "application/json")
			.header("X-Pinecone-API-Version", "2025-04") // Use the version from TS code
			.json(&payload)
			.send()
			.await
			.map_err(|e| McpError::internal_error(format!("Reqwest error: {}", e), None))?;

		if response.status().is_success() {
			let response_body = response
				.text()
				.await
				.map_err(|e| McpError::internal_error(format!("Failed to read response body: {}", e), None))?;
			Ok(response_body)
		} else {
			let status = response.status();
			let error_body = response
				.text()
				.await
				.unwrap_or_else(|_| "Could not read error body".to_string());
			Err(McpError::internal_error(
				format!("Pinecone API error ({}): {}", status, error_body),
				None,
			))
		}
	}
}

// Define tools
#[tool(tool_box)]
impl ArgonMcpServer {
	#[tool(description = "Query the Roblox Developer Forum via Pinecone.")]
	async fn roblox_developer_forum(&self, #[tool(aggr)] args: PineconeQueryArgs) -> Result<CallToolResult, McpError> {
		self.send_log(
			LoggingLevel::Info,
			&format!("Calling roblox-developer-forum with query: {}", args.query),
		);
		let response_body = self.call_pinecone_assistant("roblox-assistant", &args.query, 5).await?;
		Ok(CallToolResult {
			content: vec![Content::text(response_body)],
			is_error: Some(false),
		})
	}

	#[tool(description = "Query Luau documentation via Pinecone.")]
	async fn luau_documentation(&self, #[tool(aggr)] args: PineconeQueryArgs) -> Result<CallToolResult, McpError> {
		self.send_log(
			LoggingLevel::Info,
			&format!("Calling luau-documentation with query: {}", args.query),
		);
		let response_body = self
			.call_pinecone_assistant("roblox-luau-assistant", &args.query, 5)
			.await?;
		Ok(CallToolResult {
			content: vec![Content::text(response_body)],
			is_error: Some(false),
		})
	}

	#[tool(description = "Query Roblox engine documentation via Pinecone.")]
	async fn roblox_engine_documentation(
		&self,
		#[tool(aggr)] args: PineconeQueryArgs,
	) -> Result<CallToolResult, McpError> {
		self.send_log(
			LoggingLevel::Info,
			&format!("Calling roblox-engine-documentation with query: {}", args.query),
		);
		// Note: top_k = 1 for this assistant
		let response_body = self
			.call_pinecone_assistant("roblox-engine-reference-assistant", &args.query, 1)
			.await?;
		Ok(CallToolResult {
			content: vec![Content::text(response_body)],
			is_error: Some(false),
		})
	}
}

// Implement the server handler trait
#[tool(tool_box)]
impl ServerHandler for ArgonMcpServer {
	fn get_info(&self) -> ServerInfo {
		self.send_log(LoggingLevel::Debug, "Getting server info");

		let capabilities = ServerCapabilities::builder().enable_tools().enable_logging().build();

		ServerInfo {
			protocol_version: ProtocolVersion::V_2024_11_05,
			capabilities,
			server_info: Implementation {
				name: "argon".to_string(),
				version: env!("CARGO_PKG_VERSION").to_string(),
			},
			instructions: Some("This server provides tools for Roblox development in argon".to_string()),
		}
	}

	async fn list_resources(
		&self,
		_request: PaginatedRequestParam,
		_context: RequestContext<RoleServer>,
	) -> Result<ListResourcesResult, McpError> {
		Ok(ListResourcesResult {
			resources: vec![],
			next_cursor: None,
		})
	}

	async fn read_resource(
		&self,
		request: ReadResourceRequestParam,
		_context: RequestContext<RoleServer>,
	) -> Result<ReadResourceResult, McpError> {
		Err(McpError::resource_not_found(
			format!("Resource URI not found: {}", request.uri),
			Some(json!({ "uri": request.uri })),
		))
	}

	async fn list_prompts(
		&self,
		_request: PaginatedRequestParam,
		_context: RequestContext<RoleServer>,
	) -> Result<ListPromptsResult, McpError> {
		Ok(ListPromptsResult {
			next_cursor: None,
			prompts: Vec::new(),
		})
	}

	async fn get_prompt(
		&self,
		request: GetPromptRequestParam,
		_context: RequestContext<RoleServer>,
	) -> Result<GetPromptResult, McpError> {
		Err(McpError::invalid_params(
			format!("Prompt not found: {}", request.name),
			None,
		))
	}

	async fn list_resource_templates(
		&self,
		_request: PaginatedRequestParam,
		_context: RequestContext<RoleServer>,
	) -> Result<ListResourceTemplatesResult, McpError> {
		Ok(ListResourceTemplatesResult {
			next_cursor: None,
			resource_templates: Vec::new(),
		})
	}
}

impl ConnectMcp {
	pub fn main(self) -> Result<()> {
		// Create a new tokio runtime
		let rt = tokio::runtime::Runtime::new()?;

		// Run the server in the runtime
		rt.block_on(async { self.run_server().await })
	}

	async fn run_server(&self) -> Result<()> {
		if self.client {
			info!("Client mode is not supported in this implementation, running in server mode.");
		}

		// Remove log referencing unused host/port
		eprintln!("Starting Argon MCP Server (stdio mode)...");

		// Create server instance (Removed host and port arguments)
		let server = ArgonMcpServer::new();

		// Start MCP server using stdio transport
		// The ServiceExt::serve method creates a Peer instance from the transport
		// and stores it in server.peer during initialization. The server.peer field
		// starts as None but is populated during the initialization handshake.
		// This is provided by the rmcp crate's ServiceExt extension trait.
		let server_handle = server.serve(stdio()).await.map_err(|e| {
			eprintln!("Failed to start server: {:?}", e);
			anyhow::anyhow!("Server start failed: {}", e)
		})?;

		eprintln!("Argon MCP Server running...");

		// Wait for the server to complete
		server_handle.waiting().await.map_err(|e| {
			eprintln!("Server encountered an error while running: {:?}", e);
			anyhow::anyhow!("Server runtime failed: {}", e)
		})?;

		eprintln!("Server stopped.");
		Ok(())
	}
}
