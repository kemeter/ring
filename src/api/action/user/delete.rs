use axum::{
    extract::{Extension, Path},
    http::StatusCode,
    response::IntoResponse
};

use crate::api::server::Db;
use crate::models::users;
use crate::models::users::User;

pub(crate) async fn delete(Path(id): Path<String>, Extension(connexion): Extension<Db>, _user: User) -> impl IntoResponse {
    let guard = connexion.lock().await;
    let option = users::find(&guard, id);

    match option {
        Ok(Some(user)) => {
            users::delete(&guard, &user);

            StatusCode::NO_CONTENT
        }
        Ok(None) => {
            StatusCode::NOT_FOUND
        }

        Err(_) => {
            StatusCode::NO_CONTENT
        }
    }
}
