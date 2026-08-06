//! REST API server, routes, `OpenAPI` documentation, and SSE events.
//!
//! The API is served by axum on a TCP listener. All endpoints are documented
//! with `OpenAPI` via `utoipa` and served with Swagger UI at `/docs`.

pub mod router;
pub mod routes;
pub mod sse;
pub mod ws;
