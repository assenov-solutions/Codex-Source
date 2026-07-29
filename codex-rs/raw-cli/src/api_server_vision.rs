use crate::api_server_wire::ApiError;

pub(crate) const MAX_IMAGE_INPUTS: usize = 10;

const MAX_IMAGE_URL_BYTES: usize = 3 * 1024 * 1024;
const MAX_REMOTE_IMAGE_URL_BYTES: usize = 8 * 1024;
const DATA_IMAGE_PREFIXES: [&str; 5] = [
    "data:image/png;base64,",
    "data:image/jpeg;base64,",
    "data:image/jpg;base64,",
    "data:image/webp;base64,",
    "data:image/gif;base64,",
];

pub(crate) fn validate_and_count_image(
    image_url: &str,
    image_count: &mut usize,
    param: &str,
) -> Result<(), ApiError> {
    if *image_count >= MAX_IMAGE_INPUTS {
        return Err(ApiError::invalid(
            format!("At most {MAX_IMAGE_INPUTS} image inputs are supported per request."),
            Some(param),
        ));
    }
    validate_image_url(image_url, param)?;
    *image_count += 1;
    Ok(())
}

fn validate_image_url(image_url: &str, param: &str) -> Result<(), ApiError> {
    if let Some(remote_url) = image_url.strip_prefix("https://") {
        if remote_url.is_empty() || image_url.len() > MAX_REMOTE_IMAGE_URL_BYTES {
            return Err(invalid_image_url(param));
        }
        return Ok(());
    }

    let Some(encoded) = DATA_IMAGE_PREFIXES
        .iter()
        .find_map(|prefix| image_url.strip_prefix(prefix))
    else {
        return Err(invalid_image_url(param));
    };
    if encoded.is_empty()
        || image_url.len() > MAX_IMAGE_URL_BYTES
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
    {
        return Err(invalid_image_url(param));
    }
    Ok(())
}

fn invalid_image_url(param: &str) -> ApiError {
    ApiError::invalid(
        "Image inputs must use HTTPS or a base64 PNG, JPEG, WEBP, or GIF data URL.",
        Some(param),
    )
}
