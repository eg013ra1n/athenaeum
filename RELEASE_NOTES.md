## What's New

- Docker / self-hosted web deployment — full Axum HTTP backend with Docker Compose
- Configurable memory cache for blink viewer — size (10-5000 images) and retention (1-1440 min)
- Configurable concurrent processing threads for image rendering
- Web folder browser modal for directory selection in web/Docker mode
- Beta update channel — opt-in checkbox in Settings to check for beta releases

## Changes

- Default blink cache mode switched from disk to memory (faster frame navigation, ~60 MB RAM for 200 cached images)
- Blink viewer uses a unified priority queue — all frames queued from current position forward, replacing the old two-phase sequential+concurrent preload
- Desktop-only UI elements (cache mode toggle, file manager reveal, scan root buttons) hidden in web mode
- Web concurrency control — image processing semaphore limits parallel FITS rendering in Docker deployments

## Bug Fixes

- Fixed camelCase/snake_case serialization mismatches causing HTTP 422 errors in 34 web API routes
- Fixed binary image delivery in web backend (returns raw JPEG instead of JSON-wrapped)
- Fixed export folder path selection and validation for Docker volume mounts
- Fixed blink thread setting key mismatch in web backend (blink.max_threads → blink.threads)
