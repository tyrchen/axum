use crate::{
    body::{Body, HttpBody},
    response::{Redirect, Response},
    routing::Fallback,
    routing::{future::RouteFuture, replace_path},
    Router,
};
use arc_swap::ArcSwap;
use axum_core::response::IntoResponse;
use http::Request;
use matchit::MatchError;
use std::{
    convert::Infallible,
    ops::Deref,
    sync::Arc,
    task::{Context, Poll},
};
use tower::Service;

/// A [`Route Service`] that can be reloaded at runtime.
#[derive(Debug)]
pub struct ReloadableRouterService<B = Body> {
    inner: Arc<ArcSwap<Router<B>>>,
}

/// TODO: This is unsafe!!!! just to make it compile
unsafe impl<B> Send for ReloadableRouterService<B> {}

impl<B> Deref for ReloadableRouterService<B> {
    type Target = Arc<ArcSwap<Router<B>>>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<B> Clone for ReloadableRouterService<B> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<B> Default for ReloadableRouterService<B>
where
    B: HttpBody + Send + 'static,
{
    fn default() -> Self {
        Self {
            inner: Arc::new(ArcSwap::from_pointee(Router::default())),
        }
    }
}

impl<B> From<Router<B>> for ReloadableRouterService<B>
where
    B: HttpBody + Send + 'static,
{
    fn from(svc: Router<B>) -> Self {
        Self {
            inner: Arc::new(ArcSwap::from_pointee(svc)),
        }
    }
}

impl<B> Service<Request<B>> for ReloadableRouterService<B>
where
    B: HttpBody + Send + 'static,
{
    type Response = Response;
    type Error = Infallible;
    type Future = RouteFuture<B, Infallible>;

    #[inline]
    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    #[inline]
    fn call(&mut self, mut req: Request<B>) -> Self::Future {
        #[cfg(feature = "original-uri")]
        {
            use crate::extract::OriginalUri;

            if req.extensions().get::<OriginalUri>().is_none() {
                let original_uri = OriginalUri(req.uri().clone());
                req.extensions_mut().insert(original_uri);
            }
        }

        let path = req.uri().path().to_owned();
        let this = self.load_full();

        match this.node.at(&path) {
            Ok(match_) => this.call_route(match_, req),
            Err(err) => {
                let mut fallback = match &this.fallback {
                    Fallback::Default(inner) => inner.clone(),
                    Fallback::Custom(inner) => inner.clone(),
                };

                let new_uri = match err {
                    MatchError::MissingTrailingSlash => {
                        replace_path(req.uri(), &format!("{}/", &path))
                    }
                    MatchError::ExtraTrailingSlash => {
                        replace_path(req.uri(), path.strip_suffix('/').unwrap())
                    }
                    MatchError::NotFound => None,
                };

                if let Some(new_uri) = new_uri {
                    RouteFuture::from_response(
                        Redirect::permanent(&new_uri.to_string()).into_response(),
                    )
                } else {
                    fallback.call(req)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::{routing::get, test_helpers::TestClient};
    use http::StatusCode;
    use hyper::Body;

    #[tokio::test]
    async fn reloadable_routing() {
        let app: ReloadableRouterService = Router::new()
            .route(
                "/users",
                get(|_: Request<Body>| async { "users#index" })
                    .post(|_: Request<Body>| async { "users#create" }),
            )
            .route("/users/:id", get(|_: Request<Body>| async { "users#show" }))
            .route(
                "/users/:id/action",
                get(|_: Request<Body>| async { "users#action" }),
            )
            .into();

        let client = TestClient::new(app.clone());

        let res = client.get("/").send().await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);

        let res = client.get("/users").send().await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.text().await, "users#index");

        let res = client.post("/users").send().await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.text().await, "users#create");

        let res = client.get("/users/1").send().await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.text().await, "users#show");

        let res = client.get("/users/1/action").send().await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.text().await, "users#action");

        let new_router = Router::new().route(
            "/users",
            get(|_: Request<Body>| async { "users#index" })
                .post(|_: Request<Body>| async { "users#new" }),
        );
        app.store(Arc::new(new_router));
        let res = client.post("/users").send().await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.text().await, "users#new");
        let res = client.get("/users/1/action").send().await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }
}
