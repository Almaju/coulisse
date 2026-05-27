#[cfg(feature = "s3")]
mod inner {
    use std::pin::Pin;

    use aws_config::BehaviorVersion;
    use aws_sdk_s3::Client;
    use aws_sdk_s3::config::Region;

    use crate::backend::Backend;
    use crate::config::S3Config;
    use crate::error::StorageError;

    /// S3-compatible blob store (AWS, MinIO, Cloudflare R2).
    pub struct S3Backend {
        bucket: String,
        client: Client,
    }

    impl S3Backend {
        /// Build an S3 client from the YAML config and the ambient credential chain.
        ///
        /// # Errors
        ///
        /// Returns an error if the AWS SDK fails to load credentials.
        pub async fn new(cfg: &S3Config) -> Result<Self, StorageError> {
            let mut loader = aws_config::defaults(BehaviorVersion::latest())
                .region(Region::new(cfg.region.clone()));
            if let Some(endpoint) = &cfg.endpoint_url {
                loader = loader.endpoint_url(endpoint.clone());
            }
            let sdk_config = loader.load().await;
            let mut s3_cfg = aws_sdk_s3::config::Builder::from(&sdk_config);
            if cfg.endpoint_url.is_some() {
                // WHY: path-style is required for MinIO/local S3 endpoints that
                // don't support virtual-hosted-style bucket addressing.
                s3_cfg = s3_cfg.force_path_style(true);
            }
            let client = Client::from_conf(s3_cfg.build());
            Ok(Self {
                bucket: cfg.bucket.clone(),
                client,
            })
        }
    }

    impl Backend for S3Backend {
        fn put<'a>(
            &'a self,
            key: &'a str,
            data: &'a [u8],
        ) -> Pin<Box<dyn std::future::Future<Output = Result<(), StorageError>> + Send + 'a>>
        {
            Box::pin(async move {
                self.client
                    .put_object()
                    .bucket(&self.bucket)
                    .key(key)
                    .body(data.to_vec().into())
                    .send()
                    .await
                    .map_err(|e| StorageError::backend(format!("s3 put {key}: {e}")))?;
                Ok(())
            })
        }

        fn get<'a>(
            &'a self,
            key: &'a str,
        ) -> Pin<Box<dyn std::future::Future<Output = Result<Vec<u8>, StorageError>> + Send + 'a>>
        {
            Box::pin(async move {
                let resp = self
                    .client
                    .get_object()
                    .bucket(&self.bucket)
                    .key(key)
                    .send()
                    .await
                    .map_err(|e| {
                        // WHY: aws_sdk_s3 wraps NoSuchKey in a service error variant;
                        // surface as NotFound so callers get a consistent error type.
                        let msg = e.to_string();
                        if msg.contains("NoSuchKey") || msg.contains("no such key") {
                            StorageError::NotFound(key.to_string())
                        } else {
                            StorageError::backend(format!("s3 get {key}: {e}"))
                        }
                    })?;
                let bytes = resp
                    .body
                    .collect()
                    .await
                    .map_err(|e| StorageError::backend(format!("s3 collect {key}: {e}")))?
                    .into_bytes()
                    .to_vec();
                Ok(bytes)
            })
        }

        fn delete<'a>(
            &'a self,
            key: &'a str,
        ) -> Pin<Box<dyn std::future::Future<Output = Result<(), StorageError>> + Send + 'a>>
        {
            Box::pin(async move {
                self.client
                    .delete_object()
                    .bucket(&self.bucket)
                    .key(key)
                    .send()
                    .await
                    .map_err(|e| StorageError::backend(format!("s3 delete {key}: {e}")))?;
                Ok(())
            })
        }

        fn list_keys<'a>(
            &'a self,
        ) -> Pin<
            Box<dyn std::future::Future<Output = Result<Vec<String>, StorageError>> + Send + 'a>,
        > {
            // S3 uses lazy reconciliation via get_content; no boot scan.
            Box::pin(async move { Ok(vec![]) })
        }
    }
}

#[cfg(feature = "s3")]
pub use inner::S3Backend;
