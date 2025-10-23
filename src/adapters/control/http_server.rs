//! HTTP API 服务器
//! 
//! 提供 REST 接口用于管理代理规则、连接和拦截决策

use std::net::SocketAddr;
use std::sync::Arc;
use axum::{
    Router,
    routing::{get, post},
    extract::{State, Path, Query},
    Json,
    response::IntoResponse,
    http::StatusCode,
};
use serde::{Serialize, Deserialize};
use tracing::{info, error};

#[cfg(feature = "openapi")]
use utoipa::OpenApi;
#[cfg(feature = "openapi")]
use utoipa_swagger_ui::SwaggerUi;

use crate::core::ports::{ControlPort, AppError};
use crate::core::model::{Rule, RequestModifications, ConnectionSummary};
use super::api::ControlService;

#[cfg(feature = "openapi")]
/// OpenAPI 文档定义
#[derive(OpenApi)]
#[openapi(
    paths(
        health_check,
        get_rules,
        apply_rules,
        get_rule,
        upsert_rule,
        delete_rule,
        get_connections,
        intercept_connection,
        approve_request,
        reject_request,
        replay_request,
    ),
    components(
        schemas(ApiResponse<String>, ApiResponse<Vec<Rule>>, ApiResponse<Vec<ConnectionSummary>>)
    ),
    tags(
        (name = "health", description = "健康检查接口"),
        (name = "rules", description = "规则管理接口"),
        (name = "connections", description = "连接管理接口"),
        (name = "requests", description = "请求拦截和重放接口"),
    ),
    info(
        title = "Takeover Proxy API",
        version = "0.1.0",
        description = "透明代理管理 API",
    )
)]
struct ApiDoc;

/// HTTP API 服务器
pub struct HttpApiServer {
    listen_addr: SocketAddr,
    running: Arc<std::sync::atomic::AtomicBool>,
}

/// API 响应格式
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
struct ApiResponse<T> {
    success: bool,
    data: Option<T>,
    error: Option<String>,
}

/// 规则列表请求
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
struct RuleRequest {
    rules: Vec<Rule>,
}

/// 规则查询参数
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
struct RuleQueryParams {
    /// 按规则名称过滤
    name: Option<String>,
    /// 按规则ID过滤
    id: Option<String>,
}

/// 拦截决策请求
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
struct DecisionRequest {
    #[allow(dead_code)]
    decision: String,
    modifications: Option<RequestModifications>,
}

/// 重放请求
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
struct ReplayRequest {
    modifications: RequestModifications,
}

impl HttpApiServer {
    pub fn new(listen_addr: SocketAddr) -> Self {
        Self {
            listen_addr,
            running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
    
    pub async fn start(&self) -> Result<(), AppError> {
        info!("🚀 Starting HTTP API server on {}", self.listen_addr);
        
        let app_core = crate::core::AppCore::global();
        let control = app_core.state::<ControlService>().await
            .ok_or_else(|| AppError::Config("ControlService not found".to_string()))?;
        
        let mut app = Router::new()
            .route("/health", get(health_check))
            .route("/api/rules", get(get_rules).post(apply_rules))
            .route("/api/rules/{id}", get(get_rule).put(upsert_rule).delete(delete_rule))
            .route("/api/connections", get(get_connections))
            .route("/api/connections/{id}/intercept", post(intercept_connection))
            .route("/api/requests/{id}/approve", post(approve_request))
            .route("/api/requests/{id}/reject", post(reject_request))
            .route("/api/requests/{id}/replay", post(replay_request));
        
        #[cfg(feature = "openapi")]
        {
            app = app.merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()));
        }
        
        let app = app.with_state(control);
        
        self.running.store(true, std::sync::atomic::Ordering::SeqCst);
        
        let listener = tokio::net::TcpListener::bind(self.listen_addr).await
            .map_err(|e| AppError::Config(format!("Failed to bind: {}", e)))?;
        
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                error!("❌ HTTP server error: {}", e);
            }
        });
        
        Ok(())
    }
    
    pub async fn stop(&self) -> Result<(), AppError> {
        info!("🛑 Stopping HTTP API server");
        self.running.store(false, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    
    pub fn is_running(&self) -> bool {
        self.running.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Default for HttpApiServer {
    fn default() -> Self {
        Self::new("127.0.0.1:8768".parse().unwrap())
    }
}

// ============================================================================
// HTTP Handlers
// ============================================================================

/// 健康检查
#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/health",
    tag = "health",
    responses(
        (status = 200, description = "服务正常", body = ApiResponse<String>)
    )
))]
async fn health_check() -> impl IntoResponse {
    Json(ApiResponse::<String> {
        success: true,
        data: Some("OK".to_string()),
        error: None,
    })
}

/// 获取规则列表（支持查询参数过滤）
#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/api/rules",
    tag = "rules",
    params(RuleQueryParams),
    responses(
        (status = 200, description = "成功获取规则列表", body = ApiResponse<Vec<Rule>>),
        (status = 500, description = "服务器错误")
    )
))]
async fn get_rules(
    State(control): State<Arc<ControlService>>,
    Query(params): Query<RuleQueryParams>,
) -> impl IntoResponse {
    // 如果有查询参数，使用单规则查询
    if let Some(name_pattern) = params.name {
        match control.get_rules_by_name(&name_pattern).await {
            Ok(rules) => {
                if rules.is_empty() {
                    return (StatusCode::NOT_FOUND, Json(ApiResponse::<Vec<Rule>> {
                        success: false,
                        data: None,
                        error: Some(format!("No rules found matching name pattern '{}'", name_pattern)),
                    }))
                }
                return (StatusCode::OK, Json(ApiResponse {
                    success: true,
                    data: Some(rules),
                    error: None,
                }))
            }
            Err(e) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<Vec<Rule>> {
                    success: false,
                    data: None,
                    error: Some(e.to_string()),
                }))
            }
        }
    }
    
    if let Some(id) = params.id {
        match control.get_rule_by_id(&id).await {
            Ok(Some(rule)) => {
                return (StatusCode::OK, Json(ApiResponse {
                    success: true,
                    data: Some(vec![rule]),
                    error: None,
                }))
            }
            Ok(None) => {
                return (StatusCode::NOT_FOUND, Json(ApiResponse::<Vec<Rule>> {
                    success: false,
                    data: None,
                    error: Some(format!("Rule with id '{}' not found", id)),
                }))
            }
            Err(e) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<Vec<Rule>> {
                    success: false,
                    data: None,
                    error: Some(e.to_string()),
                }))
            }
        }
    }
    
    // 无查询参数，返回所有规则
    match control.get_active_rules().await {
        Ok(rules) => (StatusCode::OK, Json(ApiResponse {
            success: true,
            data: Some(rules),
            error: None,
        })),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<Vec<Rule>> {
            success: false,
            data: None,
            error: Some(e.to_string()),
        })),
    }
}

/// 应用规则（upsert语义：存在则更新，不存在则插入）
#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/rules",
    tag = "rules",
    request_body = RuleRequest,
    responses(
        (status = 200, description = "规则应用成功（upsert）", body = ApiResponse<String>),
        (status = 500, description = "服务器错误")
    )
))]
async fn apply_rules(
    State(control): State<Arc<ControlService>>,
    Json(req): Json<RuleRequest>,
) -> impl IntoResponse {
    let port_rules: Vec<crate::core::ports::Rule> = req.rules.into_iter().map(|r| r.into()).collect();
    match control.apply_rules(port_rules).await {
        Ok(_) => (StatusCode::OK, Json(ApiResponse::<String> {
            success: true,
            data: Some("Rules applied (upsert)".to_string()),
            error: None,
        })),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<String> {
            success: false,
            data: None,
            error: Some(e.to_string()),
        })),
    }
}

/// 获取单个规则
#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/api/rules/{id}",
    tag = "rules",
    params(
        ("id" = String, Path, description = "规则ID")
    ),
    responses(
        (status = 200, description = "成功获取规则", body = ApiResponse<Rule>),
        (status = 404, description = "规则未找到"),
        (status = 500, description = "服务器错误")
    )
))]
async fn get_rule(
    State(control): State<Arc<ControlService>>,
    Path(rule_id): Path<String>,
) -> impl IntoResponse {
    match control.get_rule_by_id(&rule_id).await {
        Ok(Some(rule)) => (StatusCode::OK, Json(ApiResponse {
            success: true,
            data: Some(rule),
            error: None,
        })),
        Ok(None) => (StatusCode::NOT_FOUND, Json(ApiResponse::<Rule> {
            success: false,
            data: None,
            error: Some(format!("Rule {} not found", rule_id)),
        })),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<Rule> {
            success: false,
            data: None,
            error: Some(e.to_string()),
        })),
    }
}

/// 创建或更新单个规则（upsert）
#[cfg_attr(feature = "openapi", utoipa::path(
    put,
    path = "/api/rules/{id}",
    tag = "rules",
    params(
        ("id" = String, Path, description = "规则ID")
    ),
    request_body = Rule,
    responses(
        (status = 200, description = "规则创建或更新成功", body = ApiResponse<Rule>),
        (status = 400, description = "请求参数错误"),
        (status = 500, description = "服务器错误")
    )
))]
async fn upsert_rule(
    State(control): State<Arc<ControlService>>,
    Path(rule_id): Path<String>,
    Json(rule): Json<Rule>,
) -> impl IntoResponse {
    // 确保路径参数ID和body中的ID一致
    if rule.id != rule_id {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse::<Rule> {
            success: false,
            data: None,
            error: Some("Rule ID in path and body must match".to_string()),
        }));
    }
    
    match control.upsert_rule(rule).await {
        Ok(saved_rule) => (StatusCode::OK, Json(ApiResponse {
            success: true,
            data: Some(saved_rule),
            error: None,
        })),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<Rule> {
            success: false,
            data: None,
            error: Some(e.to_string()),
        })),
    }
}

/// 删除规则
#[cfg_attr(feature = "openapi", utoipa::path(
    delete,
    path = "/api/rules/{id}",
    tag = "rules",
    params(
        ("id" = String, Path, description = "规则ID")
    ),
    responses(
        (status = 200, description = "规则删除成功", body = ApiResponse<String>),
        (status = 404, description = "规则未找到"),
        (status = 500, description = "服务器错误")
    )
))]
async fn delete_rule(
    State(control): State<Arc<ControlService>>,
    Path(rule_id): Path<String>,
) -> impl IntoResponse {
    match control.delete_rule(&rule_id).await {
        Ok(_) => (StatusCode::OK, Json(ApiResponse::<String> {
            success: true,
            data: Some(format!("Rule {} deleted", rule_id)),
            error: None,
        })),
        Err(e) if e.to_string().contains("not found") => {
            (StatusCode::NOT_FOUND, Json(ApiResponse::<String> {
                success: false,
                data: None,
                error: Some(e.to_string()),
            }))
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<String> {
            success: false,
            data: None,
            error: Some(e.to_string()),
        })),
    }
}

/// 获取连接列表
#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/api/connections",
    tag = "connections",
    responses(
        (status = 200, description = "成功获取连接列表", body = ApiResponse<Vec<ConnectionSummary>>),
        (status = 500, description = "服务器错误")
    )
))]
async fn get_connections(
    State(control): State<Arc<ControlService>>,
) -> impl IntoResponse {
    match control.get_active_connections().await {
        Ok(conns) => (StatusCode::OK, Json(ApiResponse {
            success: true,
            data: Some(conns),
            error: None,
        })),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<Vec<ConnectionSummary>> {
            success: false,
            data: None,
            error: Some(e.to_string()),
        })),
    }
}

/// 拦截连接
#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/connections/{id}/intercept",
    tag = "connections",
    params(
        ("id" = String, Path, description = "连接ID")
    ),
    responses(
        (status = 200, description = "连接拦截成功", body = ApiResponse<String>),
        (status = 500, description = "服务器错误")
    )
))]
async fn intercept_connection(
    State(control): State<Arc<ControlService>>,
    Path(conn_id): Path<String>,
) -> impl IntoResponse {
    match control.intercept_connection(conn_id).await {
        Ok(_) => (StatusCode::OK, Json(ApiResponse::<String> {
            success: true,
            data: Some("Connection intercepted".to_string()),
            error: None,
        })),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<String> {
            success: false,
            data: None,
            error: Some(e.to_string()),
        })),
    }
}

/// 批准请求
#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/requests/{id}/approve",
    tag = "requests",
    params(
        ("id" = String, Path, description = "请求ID")
    ),
    request_body = DecisionRequest,
    responses(
        (status = 200, description = "请求已批准", body = ApiResponse<String>),
        (status = 500, description = "服务器错误")
    )
))]
async fn approve_request(
    State(control): State<Arc<ControlService>>,
    Path(req_id): Path<String>,
    Json(req): Json<DecisionRequest>,
) -> impl IntoResponse {
    match control.approve_request(req_id, req.modifications).await {
        Ok(_) => (StatusCode::OK, Json(ApiResponse::<String> {
            success: true,
            data: Some("Request approved".to_string()),
            error: None,
        })),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<String> {
            success: false,
            data: None,
            error: Some(e.to_string()),
        })),
    }
}

/// 拒绝请求
#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/requests/{id}/reject",
    tag = "requests",
    params(
        ("id" = String, Path, description = "请求ID")
    ),
    request_body = DecisionRequest,
    responses(
        (status = 200, description = "请求已拒绝", body = ApiResponse<String>),
        (status = 500, description = "服务器错误")
    )
))]
async fn reject_request(
    State(control): State<Arc<ControlService>>,
    Path(req_id): Path<String>,
    Json(req): Json<DecisionRequest>,
) -> impl IntoResponse {
    let reason = req.modifications.map(|m| format!("{:?}", m)).unwrap_or_default();
    match control.reject_request(req_id, reason).await {
        Ok(_) => (StatusCode::OK, Json(ApiResponse::<String> {
            success: true,
            data: Some("Request rejected".to_string()),
            error: None,
        })),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<String> {
            success: false,
            data: None,
            error: Some(e.to_string()),
        })),
    }
}

/// 重放请求
#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/requests/{id}/replay",
    tag = "requests",
    params(
        ("id" = String, Path, description = "请求ID")
    ),
    request_body = ReplayRequest,
    responses(
        (status = 200, description = "请求重放成功"),
        (status = 500, description = "服务器错误")
    )
))]
async fn replay_request(
    State(control): State<Arc<ControlService>>,
    Path(req_id): Path<String>,
    Json(req): Json<ReplayRequest>,
) -> impl IntoResponse {
    use crate::core::ports::ReplayResult;
    match control.trigger_replay(req_id, req.modifications).await {
        Ok(result) => (StatusCode::OK, Json(ApiResponse {
            success: true,
            data: Some(result),
            error: None,
        })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<ReplayResult> {
            success: false,
            data: None,
            error: Some(e.to_string()),
        })).into_response(),
    }
}
