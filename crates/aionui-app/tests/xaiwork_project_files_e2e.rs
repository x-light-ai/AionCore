// FORK-CUSTOM: XAIWork project file HTTP contract coverage stays out of upstream file tests.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

use common::{body_json, build_app, json_with_token};

struct UploadMultipart {
    boundary: String,
    parts: Vec<u8>,
}

impl UploadMultipart {
    fn new() -> Self {
        Self {
            boundary: "----TestBoundaryXaiworkProjectUpload9XyZ".to_owned(),
            parts: Vec::new(),
        }
    }

    fn add_text(mut self, name: &str, value: &str) -> Self {
        self.parts
            .extend_from_slice(format!("--{}\r\n", self.boundary).as_bytes());
        self.parts
            .extend_from_slice(format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes());
        self.parts.extend_from_slice(value.as_bytes());
        self.parts.extend_from_slice(b"\r\n");
        self
    }

    fn add_file(mut self, name: &str, filename: &str, mime: &str, data: &[u8]) -> Self {
        self.parts
            .extend_from_slice(format!("--{}\r\n", self.boundary).as_bytes());
        self.parts.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\n").as_bytes(),
        );
        self.parts
            .extend_from_slice(format!("Content-Type: {mime}\r\n\r\n").as_bytes());
        self.parts.extend_from_slice(data);
        self.parts.extend_from_slice(b"\r\n");
        self
    }

    fn build(mut self) -> (String, Vec<u8>) {
        self.parts
            .extend_from_slice(format!("--{}--\r\n", self.boundary).as_bytes());
        let content_type = format!("multipart/form-data; boundary={}", self.boundary);
        (content_type, self.parts)
    }
}

fn project_upload_request(content_type: &str, body: Vec<u8>, token: &str, csrf: &str) -> Request<Body> {
    let content_length = body.len();
    Request::builder()
        .method("POST")
        .uri("/api/xaiwork/project-files/upload")
        .header("content-type", content_type)
        .header("content-length", content_length)
        .header("authorization", format!("Bearer {token}"))
        .header("x-csrf-token", csrf)
        .header("cookie", format!("aionui-csrf-token={csrf}"))
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn project_upload_writes_nested_file_into_workspace() {
    let (app, services) = build_app().await;
    let token = services.jwt_service.sign("system_default_user", "admin").unwrap();
    let csrf = "xaiwork-upload-test-csrf".to_owned();
    let workspace = tempfile::tempdir().unwrap();
    let created = services
        .project_service
        .create_standard(
            "system_default_user",
            aionui_project::canonical::to_file_uri(workspace.path()).unwrap(),
        )
        .await
        .unwrap();
    let pe_id = created.project_explorer.pe_id;
    let (content_type, body) = UploadMultipart::new()
        .add_file("file", "readme.md", "text/markdown", b"# uploaded")
        .add_text("pe_id", &pe_id)
        .add_text("relative_path", "docs/readme.md")
        .build();

    let response = app
        .oneshot(project_upload_request(&content_type, body, &token, &csrf))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["data"]["pe_id"], pe_id);
    assert_eq!(json["data"]["relative_path"], "docs/readme.md");
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("docs/readme.md")).unwrap(),
        "# uploaded"
    );
}

#[tokio::test]
async fn project_upload_rejects_parent_traversal() {
    let (app, services) = build_app().await;
    let token = services.jwt_service.sign("system_default_user", "admin").unwrap();
    let csrf = "xaiwork-upload-test-csrf".to_owned();
    let parent = tempfile::tempdir().unwrap();
    let workspace = parent.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let created = services
        .project_service
        .create_standard(
            "system_default_user",
            aionui_project::canonical::to_file_uri(&workspace).unwrap(),
        )
        .await
        .unwrap();
    let (content_type, body) = UploadMultipart::new()
        .add_file("file", "secret.txt", "text/plain", b"secret")
        .add_text("pe_id", &created.project_explorer.pe_id)
        .add_text("relative_path", "../secret.txt")
        .build();

    let response = app
        .oneshot(project_upload_request(&content_type, body, &token, &csrf))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(!parent.path().join("secret.txt").exists());
}

#[tokio::test]
async fn project_upload_rejects_another_users_project_ref() {
    let (app, services) = build_app().await;
    let hash = aionui_auth::hash_password("StrongP@ss1").unwrap();
    let other_user = services.user_repo.create_user("other_user", &hash).await.unwrap();
    let token = services.jwt_service.sign(&other_user.id, "other_user").unwrap();
    let csrf = "xaiwork-upload-test-csrf".to_owned();
    let workspace = tempfile::tempdir().unwrap();
    let created = services
        .project_service
        .create_standard(
            "system_default_user",
            aionui_project::canonical::to_file_uri(workspace.path()).unwrap(),
        )
        .await
        .unwrap();
    let (content_type, body) = UploadMultipart::new()
        .add_file("file", "private.txt", "text/plain", b"private")
        .add_text("pe_id", &created.project_explorer.pe_id)
        .add_text("relative_path", "private.txt")
        .build();

    let response = app
        .oneshot(project_upload_request(&content_type, body, &token, &csrf))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(!workspace.path().join("private.txt").exists());
}

#[tokio::test]
async fn project_transfer_copies_directory_and_resolves_name_conflicts() {
    let (app, services) = build_app().await;
    let token = services.jwt_service.sign("system_default_user", "admin").unwrap();
    let csrf = "xaiwork-transfer-test-csrf".to_owned();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(workspace.path().join("docs/empty")).unwrap();
    std::fs::create_dir(workspace.path().join("archive")).unwrap();
    std::fs::write(workspace.path().join("docs/readme.md"), "hello").unwrap();
    let created = services
        .project_service
        .create_standard(
            "system_default_user",
            aionui_project::canonical::to_file_uri(workspace.path()).unwrap(),
        )
        .await
        .unwrap();
    let pe_id = created.project_explorer.pe_id;
    let request_body = || {
        json!({
            "source": { "pe_id": pe_id, "relative_path": "docs" },
            "target": { "pe_id": pe_id, "relative_path": "archive" },
            "operation": "copy"
        })
    };

    let first = app
        .clone()
        .oneshot(json_with_token(
            "POST",
            "/api/xaiwork/project-files/transfer",
            request_body(),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(body_json(first).await["data"]["relative_path"], "archive/docs");
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("archive/docs/readme.md")).unwrap(),
        "hello"
    );
    assert!(workspace.path().join("archive/docs/empty").is_dir());

    let second = app
        .oneshot(json_with_token(
            "POST",
            "/api/xaiwork/project-files/transfer",
            request_body(),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(body_json(second).await["data"]["relative_path"], "archive/docs(2)");
}

#[tokio::test]
async fn project_transfer_moves_file() {
    let (app, services) = build_app().await;
    let token = services.jwt_service.sign("system_default_user", "admin").unwrap();
    let csrf = "xaiwork-transfer-test-csrf".to_owned();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir(workspace.path().join("target")).unwrap();
    std::fs::write(workspace.path().join("notes.txt"), "hello").unwrap();
    let created = services
        .project_service
        .create_standard(
            "system_default_user",
            aionui_project::canonical::to_file_uri(workspace.path()).unwrap(),
        )
        .await
        .unwrap();
    let pe_id = created.project_explorer.pe_id;

    let response = app
        .oneshot(json_with_token(
            "POST",
            "/api/xaiwork/project-files/transfer",
            json!({
                "source": { "pe_id": pe_id, "relative_path": "notes.txt" },
                "target": { "pe_id": pe_id, "relative_path": "target" },
                "operation": "move"
            }),
            &token,
            &csrf,
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(!workspace.path().join("notes.txt").exists());
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("target/notes.txt")).unwrap(),
        "hello"
    );
}

#[tokio::test]
async fn project_transfer_rejects_missing_auth_or_csrf() {
    let (app, services) = build_app().await;
    let token = services.jwt_service.sign("system_default_user", "admin").unwrap();
    let body = json!({
        "source": { "pe_id": "pe-source", "relative_path": "notes.txt" },
        "target": { "pe_id": "pe-target", "relative_path": "" },
        "operation": "copy"
    });
    let without_auth = Request::builder()
        .method("POST")
        .uri("/api/xaiwork/project-files/transfer")
        .header("content-type", "application/json")
        .header("x-csrf-token", "test-csrf")
        .header("cookie", "aionui-csrf-token=test-csrf")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let without_csrf = Request::builder()
        .method("POST")
        .uri("/api/xaiwork/project-files/transfer")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();

    let auth_response = app.clone().oneshot(without_auth).await.unwrap();
    let csrf_response = app.oneshot(without_csrf).await.unwrap();

    assert_eq!(auth_response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(csrf_response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn project_download_returns_file_and_directory_archive() {
    let (app, services) = build_app().await;
    let token = services.jwt_service.sign("system_default_user", "admin").unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(workspace.path().join("docs/empty")).unwrap();
    std::fs::write(workspace.path().join("notes.txt"), "hello").unwrap();
    std::fs::write(workspace.path().join("docs/readme.md"), "archive me").unwrap();
    let created = services
        .project_service
        .create_standard(
            "system_default_user",
            aionui_project::canonical::to_file_uri(workspace.path()).unwrap(),
        )
        .await
        .unwrap();
    let pe_id = created.project_explorer.pe_id;
    let request = |relative_path: &str| {
        Request::builder()
            .uri(format!(
                "/api/xaiwork/project-files/download?pe_id={pe_id}&relative_path={relative_path}"
            ))
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap()
    };

    let file_response = app.clone().oneshot(request("notes.txt")).await.unwrap();
    assert_eq!(file_response.status(), StatusCode::OK);
    assert_eq!(
        file_response.headers()["content-disposition"],
        "attachment; filename=\"notes.txt\""
    );
    assert_eq!(file_response.into_body().collect().await.unwrap().to_bytes(), "hello");

    let archive_response = app.oneshot(request("docs")).await.unwrap();
    assert_eq!(archive_response.status(), StatusCode::OK);
    assert_eq!(archive_response.headers()["content-type"], "application/zip");
    let bytes = archive_response.into_body().collect().await.unwrap().to_bytes();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    assert!(archive.by_name("docs/empty/").is_ok());
    let mut readme = archive.by_name("docs/readme.md").unwrap();
    let mut contents = String::new();
    std::io::Read::read_to_string(&mut readme, &mut contents).unwrap();
    assert_eq!(contents, "archive me");
}

#[tokio::test]
async fn project_download_rejects_another_users_ref() {
    let (app, services) = build_app().await;
    let hash = aionui_auth::hash_password("StrongP@ss1").unwrap();
    let other_user = services
        .user_repo
        .create_user("download_other_user", &hash)
        .await
        .unwrap();
    let token = services
        .jwt_service
        .sign(&other_user.id, "download_other_user")
        .unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("private.txt"), "private").unwrap();
    let created = services
        .project_service
        .create_standard(
            "system_default_user",
            aionui_project::canonical::to_file_uri(workspace.path()).unwrap(),
        )
        .await
        .unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/xaiwork/project-files/download?pe_id={}&relative_path=private.txt",
                    created.project_explorer.pe_id
                ))
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
