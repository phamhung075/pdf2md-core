"""Enterprise circuit breaker and timeout resilience primitives."""
import concurrent.futures
from enum import Enum
import logging
import random
import threading
import time
from typing import Any, Callable, Optional

logger = logging.getLogger(__name__)


class CircuitState(str, Enum):
    CLOSED = "CLOSED"
    OPEN = "OPEN"
    HALF_OPEN = "HALF_OPEN"


class CircuitOpenError(Exception):
    """Raised when an operation is attempted while the circuit breaker is in the OPEN state."""
    pass


class ExecutionTimeoutError(Exception):
    """Raised when an operation exceeds its configured execution timeout boundary."""
    pass


class CircuitBreaker:
    """Thread-safe circuit breaker protecting external and compute-heavy dependencies."""

    def __init__(
        self,
        name: str = "default",
        failure_threshold: int = 5,
        recovery_timeout: float = 30.0,
        half_open_success_threshold: int = 2,
    ):
        self.name = name
        self.failure_threshold = failure_threshold
        self.recovery_timeout = recovery_timeout
        self.half_open_success_threshold = half_open_success_threshold

        self._state = CircuitState.CLOSED
        self._failure_count = 0
        self._success_count = 0
        self._last_failure_time: Optional[float] = None
        self._last_state_change: float = time.time()
        self._lock = threading.Lock()

    @property
    def state(self) -> CircuitState:
        with self._lock:
            self._evaluate_state()
            return self._state

    def _evaluate_state(self) -> None:
        """Internal check to see if an OPEN circuit should transition to HALF_OPEN."""
        if self._state == CircuitState.OPEN and self._last_failure_time:
            elapsed = time.time() - self._last_failure_time
            if elapsed >= self.recovery_timeout:
                logger.info(
                    "[CircuitBreaker:%s] Recovery timeout (%.1fs) elapsed; transitioning OPEN -> HALF_OPEN",
                    self.name,
                    elapsed,
                )
                self._state = CircuitState.HALF_OPEN
                self._success_count = 0
                self._last_state_change = time.time()

    def call(self, func: Callable[..., Any], *args: Any, **kwargs: Any) -> Any:
        """Executes the callable within the circuit breaker boundary."""
        with self._lock:
            self._evaluate_state()

            if self._state == CircuitState.OPEN:
                raise CircuitOpenError(
                    f"Circuit breaker '{self.name}' is OPEN. Requests blocked until recovery timeout."
                )

        try:
            result = func(*args, **kwargs)
            self._on_success()
            return result
        except Exception as exc:
            # Don't penalize circuit breaker for expected validation/client errors
            self._on_failure(exc)
            raise

    def _on_success(self) -> None:
        with self._lock:
            if self._state == CircuitState.HALF_OPEN:
                self._success_count += 1
                if self._success_count >= self.half_open_success_threshold:
                    logger.info(
                        "[CircuitBreaker:%s] Success threshold met in HALF_OPEN; transitioning -> CLOSED",
                        self.name,
                    )
                    self._state = CircuitState.CLOSED
                    self._failure_count = 0
                    self._success_count = 0
                    self._last_state_change = time.time()
            elif self._state == CircuitState.CLOSED:
                self._failure_count = 0

    def _on_failure(self, exc: Exception) -> None:
        with self._lock:
            self._last_failure_time = time.time()
            if self._state == CircuitState.HALF_OPEN:
                logger.warning(
                    "[CircuitBreaker:%s] Probe failed in HALF_OPEN (%s); tripping back -> OPEN",
                    self.name,
                    exc,
                )
                self._state = CircuitState.OPEN
                self._last_state_change = time.time()
            elif self._state == CircuitState.CLOSED:
                self._failure_count += 1
                logger.warning(
                    "[CircuitBreaker:%s] Failure recorded (%d/%d): %s",
                    self.name,
                    self._failure_count,
                    self.failure_threshold,
                    exc,
                )
                if self._failure_count >= self.failure_threshold:
                    logger.error(
                        "[CircuitBreaker:%s] Failure threshold reached (%d); tripping CLOSED -> OPEN",
                        self.name,
                        self._failure_count,
                    )
                    self._state = CircuitState.OPEN
                    self._last_state_change = time.time()

    def reset(self) -> None:
        """Manually forces the circuit breaker back to CLOSED state."""
        with self._lock:
            self._state = CircuitState.CLOSED
            self._failure_count = 0
            self._success_count = 0
            self._last_failure_time = None
            self._last_state_change = time.time()


def execute_with_timeout(
    func: Callable[..., Any],
    *args: Any,
    timeout_seconds: float = 60.0,
    **kwargs: Any,
) -> Any:
    """Executes a callable inside a thread boundary enforcing a hard timeout."""
    with concurrent.futures.ThreadPoolExecutor(max_workers=1) as executor:
        future = executor.submit(func, *args, **kwargs)
        try:
            return future.result(timeout=timeout_seconds)
        except concurrent.futures.TimeoutError as e:
            raise ExecutionTimeoutError(
                f"Operation timed out after {timeout_seconds} seconds"
            ) from e


def calculate_backoff_jitter(
    attempt: int,
    base_delay: float = 1.0,
    max_delay: float = 60.0,
    full_jitter: bool = True,
) -> float:
    """Calculates exponential backoff delay with jitter to prevent thundering herds."""
    temp = min(max_delay, base_delay * (2 ** attempt))
    if full_jitter:
        return random.uniform(0.0, temp)
    # Equal jitter
    half = temp / 2.0
    return half + random.uniform(0.0, half)
