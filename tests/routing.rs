use super::*;

#[tokio::test]
async fn stopped_python_service_returns_unavailable_without_booting_python() {
    let mut python = PythonService::new(Arc::new(RwLock::new(None)));
    let request = || Request::new(Full::new(Bytes::new()));
    assert_eq!(
        python.handle(request()).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        python.call(request()).await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn fixes_asgi_framing_header_is_ignored_without_losing_application_headers()
 {
    for name in [
        b"transfer-encoding".as_slice(),
        b"Transfer-Encoding",
        b"TRANSFER-ENCODING",
    ] {
        let response = response_from_asgi(
            200,
            vec![
                (name.to_vec(), b"gzip".to_vec()),
                // Even an invalid transfer-encoding value is ignored before parsing.
                (name.to_vec(), vec![0]),
                (b"content-encoding".to_vec(), b"identity".to_vec()),
                (b"set-cookie".to_vec(), b"first=1".to_vec()),
                (b"set-cookie".to_vec(), b"second=2".to_vec()),
            ],
            full_body(Bytes::from_static(b"plain")),
        )
        .unwrap();
        assert!(
            !response.headers().contains_key(http::header::TRANSFER_ENCODING)
        );
        assert_eq!(
            response.headers()[http::header::CONTENT_ENCODING],
            "identity"
        );
        let cookies: Vec<_> = response
            .headers()
            .get_all(http::header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect();
        assert_eq!(cookies, ["first=1", "second=2"]);
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "plain"
        );
    }
    assert!(
        response_from_asgi(
            200,
            vec![(b"set-cookie".to_vec(), vec![0])],
            full_body(Bytes::new())
        )
        .is_err()
    );
}

#[test]
fn every_http_version_reaches_the_application_with_its_host_first() {
    let host = |version, uri: &str, headers: &[(&str, &str)]| {
        let mut request = Request::builder().version(version).uri(uri);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let (parts, ()) = request.body(()).unwrap().into_parts();
        request_scope(&parts).headers
    };
    let expected = |rest: &[(&str, &str)]| {
        std::iter::once(("host", "stoa.example.com"))
            .chain(rest.iter().copied())
            .map(|(name, value)| {
                (name.as_bytes().to_vec(), value.as_bytes().to_vec())
            })
            .collect::<Vec<_>>()
    };
    // HTTP/2 and HTTP/3 carry :authority in the target and no Host header.
    for version in [http::Version::HTTP_2, http::Version::HTTP_3] {
        assert_eq!(
            host(
                version,
                "https://stoa.example.com/path",
                &[("accept", "text/html")]
            ),
            expected(&[("accept", "text/html")])
        );
    }
    // HTTP/1.1 in origin form names its host only in the header.
    assert_eq!(
        host(
            http::Version::HTTP_11,
            "/path",
            &[("accept", "text/html"), ("host", "stoa.example.com")]
        ),
        expected(&[("accept", "text/html")])
    );
}
