"""Unit tests for token-bucket rate limiter and Starlette middleware."""
import unittest
from unittest.mock import patch
from starlette.testclient import TestClient

from src.infrastructure.middleware.rate_limiter import (
    TokenBucketRateLimiter,
    RateLimitMiddleware,
)
from src.interfaces.http.gateway import app


class TestTokenBucketRateLimiter(unittest.TestCase):
    def test_anonymous_tier_limits_at_threshold(self):
        """Anonymous tier permits up to ANON_LIMIT hits, then throttles with 429 headers."""
        limiter = TokenBucketRateLimiter(redis_url=None)
        client_ip = "anon-client-50"

        # First 5 calls must pass (default ANON_LIMIT=5)
        for i in range(5):
            allowed, headers = limiter.check(client_ip, is_authenticated=False)
            self.assertTrue(allowed, f"Hit {i+1} should be permitted")
            self.assertEqual(headers["X-RateLimit-Limit"], "5")
            self.assertEqual(headers["X-RateLimit-Remaining"], str(4 - i))

        # 6th call must be blocked
        allowed, headers = limiter.check(client_ip, is_authenticated=False)
        self.assertFalse(allowed, "6th hit must be throttled")
        self.assertEqual(headers["X-RateLimit-Remaining"], "0")
        self.assertIn("Retry-After", headers)
        self.assertGreater(int(headers["Retry-After"]), 0)

    def test_authenticated_tier_allows_high_volume(self):
        """Authenticated tier (API key / Bearer token) allows up to 120 req/min."""
        limiter = TokenBucketRateLimiter(redis_url=None)
        client_key = "api_secret_user_99"

        # Send 10 calls, all must be permitted
        for i in range(10):
            allowed, headers = limiter.check(client_key, is_authenticated=True)
            self.assertTrue(allowed)
            self.assertEqual(headers["X-RateLimit-Limit"], "120")

    def test_rate_limit_headers_format(self):
        """Rate limit headers must contain valid integer representations."""
        limiter = TokenBucketRateLimiter(redis_url=None)
        _, headers = limiter.check("test-user-header", is_authenticated=False)
        self.assertIn("X-RateLimit-Limit", headers)
        self.assertIn("X-RateLimit-Remaining", headers)
        self.assertIn("X-RateLimit-Reset", headers)
        self.assertTrue(headers["X-RateLimit-Limit"].isdigit())
        self.assertTrue(headers["X-RateLimit-Remaining"].isdigit())
        self.assertTrue(headers["X-RateLimit-Reset"].isdigit())

    def test_middleware_returns_429_json_response(self):
        """When enabled, middleware returns HTTP 429 JSON response on quota exhaustion."""
        test_client = TestClient(app)

        with patch("src.infrastructure.middleware.rate_limiter.RATE_LIMIT_ENABLED", True):
            # Exhaust quota on /health or standard endpoint
            client_headers = {"X-Forwarded-For": "203.0.113.99"}

            # Send 5 requests to consume anonymous quota
            for _ in range(5):
                test_client.get("/", headers=client_headers)

            # 6th request should hit 429
            resp = test_client.get("/", headers=client_headers)
            self.assertEqual(resp.status_code, 429)
            data = resp.json()
            self.assertEqual(data["error"], "Too Many Requests")
            self.assertIn("retry_after_seconds", data)
            self.assertEqual(resp.headers.get("x-ratelimit-remaining"), "0")


if __name__ == "__main__":
    unittest.main()
