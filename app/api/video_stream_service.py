import asyncio
import hashlib
import logging
import os
import sys
from contextlib import asynccontextmanager
from datetime import datetime
from pathlib import Path
from typing import TypedDict

import aiofiles
from cachetools import TTLCache
from dotenv import find_dotenv, load_dotenv
from fastapi import FastAPI, HTTPException, Query, Request
from fastapi.responses import Response, StreamingResponse
from fastapi.staticfiles import StaticFiles

from app.core.runtime.paths import default_recordings_dir

from .video_stream_utils import (
    STREAM_CHUNK_SIZE,
    InvalidByteRangeError,
    InvalidVideoPathError,
    file_sender_range,
    parse_range_header,
    resolve_video_path,
)

dotenv_path = find_dotenv()
load_dotenv(dotenv_path)
CUSTOM_VIDEO_ROOT_DIR = os.getenv("CUSTOM_VIDEO_ROOT_DIR")
VIDEO_API_PORT = os.getenv("VIDEO_API_PORT") or 6007

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

DEFAULT_VIDEO_ROOT_DIR = default_recordings_dir
VIDEO_DIR = Path(CUSTOM_VIDEO_ROOT_DIR or DEFAULT_VIDEO_ROOT_DIR)
os.makedirs(VIDEO_DIR, exist_ok=True)


class VideoMeta(TypedDict):
    etag: str
    last_modified: str
    file_size: int


VIDEO_META_CACHE = TTLCache[str, VideoMeta](maxsize=50, ttl=300)

if sys.platform == "win32":
    asyncio.set_event_loop_policy(asyncio.WindowsSelectorEventLoopPolicy())


@asynccontextmanager
async def lifespan(_app: FastAPI):
    if not VIDEO_DIR.exists():
        logger.error(f"Video directory does not exist: {VIDEO_DIR}")
        raise RuntimeError(f"Video directory does not exist: {VIDEO_DIR}")
    _app.mount("/api/videos", StaticFiles(directory=VIDEO_DIR), name="videos")
    yield

    tasks = [t for t in asyncio.all_tasks() if t is not asyncio.current_task()]
    for task in tasks:
        task.cancel()
    await asyncio.gather(*tasks, return_exceptions=True)

    _app.mount("/api/videos", StaticFiles(directory=None))
    logger.info("Shutting down the application.")


app = FastAPI(lifespan=lifespan)


@app.get("/api/videos")
async def get_video(request: Request, filename: str = Query(...), subfolder: str | None = None):

    cache_key = f"{filename}-{subfolder}"
    if meta := VIDEO_META_CACHE.get(cache_key):
        if_none_match = request.headers.get("If-None-Match")
        if_modified_since = request.headers.get("If-Modified-Since")

        if if_none_match and if_none_match == meta["etag"]:
            return Response(status_code=304)

        if if_modified_since:
            last_modified = datetime.fromisoformat(meta["last_modified"])
            if datetime.strptime(if_modified_since, "%a, %d %b %Y %H:%M:%S GMT") >= last_modified:
                return Response(status_code=304)

    try:
        video_path = resolve_video_path(VIDEO_DIR, filename, subfolder)
    except InvalidVideoPathError as exc:
        logger.warning("Invalid video path: %s/%s", subfolder, filename)
        raise HTTPException(status_code=400, detail="Invalid file path") from exc

    if not video_path.is_file():
        logger.error(f"File not found: {video_path}")
        raise HTTPException(status_code=404, detail="Video file not found")

    stat = video_path.stat()
    file_size = stat.st_size
    last_modified = datetime.fromtimestamp(stat.st_mtime).isoformat()
    etag = hashlib.md5(f"{file_size}-{last_modified}".encode()).hexdigest()

    VIDEO_META_CACHE[cache_key] = {"etag": etag, "last_modified": last_modified, "file_size": file_size}

    # Parse Range header
    range_header = request.headers.get("Range")
    if range_header:
        try:
            start, end = parse_range_header(range_header, file_size)
        except InvalidByteRangeError as exc:
            raise HTTPException(
                status_code=416,
                detail="Requested range not satisfiable",
                headers={"Content-Range": f"bytes */{file_size}"},
            ) from exc

        headers = {
            "Content-Range": f"bytes {start}-{end}/{file_size}",
            "Accept-Ranges": "bytes",
            "Content-Length": str(end - start + 1),
            "Content-Type": "video/mp4",
        }
        return StreamingResponse(
            file_sender_range(video_path, start, end),
            status_code=206,
            headers=headers,
        )

    # If no Range header, return the whole file
    headers = {
        "Content-Length": str(file_size),
        "Content-Type": "video/mp4",
        "Accept-Ranges": "bytes",
        "Cache-Control": "public, max-age=300",
        "ETag": etag,
        "Last-Modified": datetime.fromisoformat(last_modified).strftime("%a, %d %b %Y %H:%M:%S GMT"),
    }
    try:
        return StreamingResponse(file_sender(video_path), headers=headers)
    except Exception:
        logger.exception("Streaming error")
        raise HTTPException(status_code=500, detail="Internal Server Error")


# Async file sender (full content)
async def file_sender(video_path: Path):
    async with aiofiles.open(video_path, "rb") as file:
        while True:
            chunk = await file.read(STREAM_CHUNK_SIZE)
            if not chunk:
                break
            yield chunk


if __name__ == "__main__":
    import uvicorn

    uvicorn.run(app, host="0.0.0.0", port=int(VIDEO_API_PORT), log_level="debug")
