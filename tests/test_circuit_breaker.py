"""Unit tests for Circuit Breaker, timeout boundaries, and jitter backoff."""
import time
import unittest

from src.infrastructure.resilience.circuit_breaker import (
    CircuitBreaker,
    CircuitOpenError,
    CircuitState,
    ExecutionTimeoutError,
    calculate_backoff_jitter,
    execute_with_timeout,
)


class TestCircuitBreaker(unittest.TestCase):
    def test_closed_state_success_flow(self):
        """Under normal conditions, calls pass through without state change."""
        cb = CircuitBreaker(name="test_cb", failure_threshold=3, recovery_timeout=0.2)
        result = cb.call(lambda x, y: x + y, 10, 20)
        self.assertEqual(result, 30)
        self.assertEqual(cb.state, CircuitState.CLOSED)

    def test_failure_threshold_trips_open(self):
        """Consecutive failures trip the circuit breaker into OPEN state."""
        cb = CircuitBreaker(name="test_tripper", failure_threshold=3, recovery_timeout=0.2)

        def failing_op():
            raise RuntimeError("Backend failure")

        for _ in range(3):
            with self.assertRaises(RuntimeError):
                cb.call(failing_op)

        self.assertEqual(cb.state, CircuitState.OPEN)

        # Subsequent call must immediately raise CircuitOpenError without invoking target function
        with self.assertRaises(CircuitOpenError):
            cb.call(lambda: "never called")

    def test_recovery_transitions_through_half_open_to_closed(self):
        """After recovery_timeout, breaker enters HALF_OPEN and resets to CLOSED upon success."""
        cb = CircuitBreaker(name="test_recovery", failure_threshold=2, recovery_timeout=0.05, half_open_success_threshold=2)

        # Trip to OPEN
        for _ in range(2):
            with self.assertRaises(ValueError):
                cb.call(lambda: (_ for _ in ()).throw(ValueError("boom")))

        self.assertEqual(cb.state, CircuitState.OPEN)

        # Wait for recovery timeout
        time.sleep(0.06)
        self.assertEqual(cb.state, CircuitState.HALF_OPEN)

        # First success in HALF_OPEN
        cb.call(lambda: "ok1")
        self.assertEqual(cb.state, CircuitState.HALF_OPEN)

        # Second success resets to CLOSED
        cb.call(lambda: "ok2")
        self.assertEqual(cb.state, CircuitState.CLOSED)

    def test_half_open_failure_immediately_reopens(self):
        """A failure during HALF_OPEN trial re-trips the circuit back to OPEN."""
        cb = CircuitBreaker(name="test_probe_fail", failure_threshold=2, recovery_timeout=0.05)

        for _ in range(2):
            with self.assertRaises(KeyError):
                cb.call(lambda: (_ for _ in ()).throw(KeyError("missing")))

        self.assertEqual(cb.state, CircuitState.OPEN)
        time.sleep(0.06)
        self.assertEqual(cb.state, CircuitState.HALF_OPEN)

        # Failure in HALF_OPEN
        with self.assertRaises(KeyError):
            cb.call(lambda: (_ for _ in ()).throw(KeyError("probe error")))

        self.assertEqual(cb.state, CircuitState.OPEN)

    def test_execute_with_timeout_succeeds_fast(self):
        """Operations within timeout complete successfully."""
        res = execute_with_timeout(lambda: 42 * 2, timeout_seconds=1.0)
        self.assertEqual(res, 84)

    def test_execute_with_timeout_aborts_slow_task(self):
        """Operations exceeding the timeout boundary raise ExecutionTimeoutError."""
        def slow_task():
            time.sleep(0.2)
            return "done"

        with self.assertRaises(ExecutionTimeoutError):
            execute_with_timeout(slow_task, timeout_seconds=0.05)

    def test_calculate_backoff_jitter_bounds(self):
        """Jitter backoff delay stays within [0, max_delay] bounds."""
        for attempt in range(5):
            delay = calculate_backoff_jitter(attempt, base_delay=0.5, max_delay=10.0, full_jitter=True)
            self.assertGreaterEqual(delay, 0.0)
            self.assertLessEqual(delay, 10.0)


if __name__ == "__main__":
    unittest.main()
