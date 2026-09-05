"""Token-bucket and sliding-window rate limiting middleware with Redis and in-memory fallback."""
import collections
import math
import os
import threading
import time
from typing import Callable, Dict, Optional, Tuple

from starlette.middleware.base import BaseHTTPMiddleware
from starlette.requests import Request
from starlette.responses import JSONResponse, Response

from src.infrastructure.logging.stream_logger import logger

# Tier definitions (Default to 0 in dev/test, enabled via RATE_LIMIT_ENABLED=1 in production/CapRover)
RATE_LIMIT_ENABLED = os.environ.get("RATE_LIMIT_ENABLED", "0").strip().lower() in ("1", "true", "yes", "on")

# Anonymous Sandbox Tier: 5 requests per 10 minutes (600s)
ANON_LIMIT = int(os.environ.get("RATE_LIMIT_ANON_COUNT", "5"))
ANON_WINDOW_SEC = int(os.environ.get("RATE_LIMIT_ANON_WINDOW", "600"))

# Authenticated Tier (API Key / Bearer JWT): 120 requests per 1 minute (60s)
AUTH_LIMIT = int(os.environ.get("RATE_LIMIT_AUTH_COUNT", "120"))
AUTH_WINDOW_SEC = int(os.environ.get("RATE_LIMIT_AUTH_WINDOW", "60"))

# Path prefixes that are exempt from rate limiting (health checks, static assets, docs, webhooks)
EXEMPT_PATHS = ("/health", "/docs", "/openapi.json", "/redoc", "/static", "/v1/webhooks")


class TokenBucketRateLimiter:
    """Sliding-window / token bucket rate limiter supporting Redis and thread-safe in-memory fallback."""

    def __init__(self, redis_url: Optional[str] = None):
        self._redis_url = redis_url or os.environ.get("REDIS_URL")
        self._redis_client = None
        self._memory_buckets: Dict[str, collections.deque] = collections.defaultdict(collections.deque)
        self._lock = threading.Lock()
        self._init_redis()

    def _init_redis(self) -> None:
        if not self._redis_url:
            return
        try:
            import redis
            client = redis.Redis.from_url(self._redis_url, socket_timeout=1.0, socket_connect_timeout=1.0)
            client.ping()
            self._redis_client = client
            logger.info("[RateLimiter] Connected to Redis at %s", self._redis_url)
        except Exception as e:
            logger.warning("[RateLimiter] Redis unavailable (%s); using thread-safe in-memory rate limiter", e)
            self._redis_client = None

    def check(self, client_id: str, is_authenticated: bool = False) -> Tuple[bool, Dict[str, str]]:
        """Checks if a request is permitted under the applicable tier.

        Returns:
            Tuple of (is_allowed: bool, headers: dict)
        """
        limit = AUTH_LIMIT if is_authenticated else ANON_LIMIT
        window = AUTH_WINDOW_SEC if is_authenticated else ANON_WINDOW_SEC
        tier_name = "auth" if is_authenticated else "anon"
        bucket_key = f"ratelimit:{tier_name}:{client_id}"
        now = time.time()

        if self._redis_client:
            try:
                return self._check_redis(bucket_key, limit, window, now)
            except Exception as e:
                logger.warning("[RateLimiter] Redis check failed (%s); falling back to memory", e)
                # Fall through to in-memory check below

        return self._check_memory(bucket_key, limit, window, now)

    def _check_redis(self, bucket_key: str, limit: int, window: int, now: float) -> Tuple[bool, Dict[str, str]]:
        pipe = self._redis_client.pipeline()
        clear_before = now - window
        pipe.zremrangebyscore(bucket_key, 0, clear_before)
        pipe.zcard(bucket_key)
        pipe.zrange(bucket_key, 0, 0, withscores=True)
        pipe.expire(bucket_key, window)
        _, current_count, oldest_entry, _ = pipe.execute()

        if current_count < limit:
            # Under limit -> record new hit
            self._redis_client.zadd(bucket_key, {str(now): now})
            remaining = limit - (current_count + 1)
            reset_epoch = int(now + window)
            headers = {
                "X-RateLimit-Limit": str(limit),
                "X-RateLimit-Remaining": str(max(0, remaining)),
                "X-RateLimit-Reset": str(reset_epoch),
            }
            return True, headers
        else:
            # Over limit -> calculate retry after
            oldest_ts = oldest_entry[0][1] if oldest_entry else (now - window)
            reset_in = max(1, int(math.ceil(oldest_ts + window - now)))
            headers = {
                "X-RateLimit-Limit": str(limit),
                "X-RateLimit-Remaining": "0",
                "X-RateLimit-Reset": str(int(now + reset_in)),
                "Retry-After": str(reset_in),
            }
            return False, headers

    def _check_memory(self, bucket_key: str, limit: int, window: int, now: float) -> Tuple[bool, Dict[str, str]]:
        with self._lock:
            bucket = self._memory_buckets[bucket_key]
            clear_before = now - window
            while bucket and bucket[0] <= clear_before:
                bucket.popleft()

            if len(bucket) < limit:
                bucket.append(now)
                remaining = limit - len(bucket)
                reset_epoch = int(now + window)
                headers = {
                    "X-RateLimit-Limit": str(limit),
                    "X-RateLimit-Remaining": str(max(0, remaining)),
                    "X-RateLimit-Reset": str(reset_epoch),
                }
                return True, headers
            else:
                oldest_ts = bucket[0]
                reset_in = max(1, int(math.ceil(oldest_ts + window - now)))
                headers = {
                    "X-RateLimit-Limit": str(limit),
                    "X-RateLimit-Remaining": "0",
                    "X-RateLimit-Reset": str(int(now + reset_in)),
                    "Retry-After": str(reset_in),
                }
                return False, headers


# Singleton default instance
rate_limiter = TokenBucketRateLimiter()


class RateLimitMiddleware(BaseHTTPMiddleware):
    """Starlette middleware enforcing API rate limits."""

    def __init__(self, app, limiter: Optional[TokenBucketRateLimiter] = None):
        super().__init__(app)
        self.limiter = limiter or rate_limiter

    async def dispatch(self, request: Request, call_next: Callable) -> Response:
        if not RATE_LIMIT_ENABLED or request.headers.get("X-Bypass-RateLimit") == "1":
            return await call_next(request)

        path = request.url.path
        if any(path.startswith(exempt) for exempt in EXEMPT_PATHS) or request.method == "OPTIONS":
            return await call_next(request)

        # Determine authentication status and identifier
        auth_header = request.headers.get("Authorization", "")
        api_key = request.headers.get("X-API-Key", "")

        is_authenticated = bool(auth_header.startswith("Bearer ") or api_key)

        if is_authenticated:
            client_id = api_key if api_key else auth_header.split(" ", 1)[1][:32]
        else:
            forwarded = request.headers.get("X-Forwarded-For")
            if forwarded:
                client_id = forwarded.split(",")[0].strip()
            else:
                client_id = request.client.host if request.client else "127.0.0.1"

        allowed, headers = self.limiter.check(client_id, is_authenticated=is_authenticated)

        if not allowed:
            retry_after = headers.get("Retry-After", "60")
            return JSONResponse(
                status_code=429,
                content={
                    "error": "Too Many Requests",
                    "message": f"Rate limit exceeded. Try again in {retry_after} seconds.",
                    "limit": headers.get("X-RateLimit-Limit"),
                    "retry_after_seconds": int(retry_after),
                },
                headers=headers,
            )

        response = await call_next(request)
        for k, v in headers.items():
            response.headers[k] = v
        return response
