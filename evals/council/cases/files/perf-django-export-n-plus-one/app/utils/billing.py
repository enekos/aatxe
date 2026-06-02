"""Wrapper around the third-party billing provider.

The provider rate-limits at 10 req/s per API key and bills $0.001 per
single-charge call vs $0.0001 per batched call (up to 100 ids). Always
prefer ``batch_charge`` for any bulk operation.
"""

from typing import List

import requests

API_BASE = "https://api.billing.example.com/v2"


class BillingClient:
    def __init__(self, api_key: str) -> None:
        self._session = requests.Session()
        self._session.headers["Authorization"] = f"Bearer {api_key}"

    def charge(self, user_id: str, cents: int) -> str:
        """Single-user charge. Use ``batch_charge`` for > 1 user."""
        r = self._session.post(
            f"{API_BASE}/charges",
            json={"userId": user_id, "amount": cents},
            timeout=10,
        )
        r.raise_for_status()
        return r.json()["transactionId"]

    def batch_charge(self, charges: List[dict]) -> List[str]:
        """Submit up to 100 charges in a single request. Returns the
        list of transaction IDs in submission order. Prefer this for
        any bulk path — see module docstring for the cost/rate-limit
        rationale.
        """
        if len(charges) > 100:
            raise ValueError("batch_charge accepts at most 100 charges")
        r = self._session.post(
            f"{API_BASE}/charges/batch",
            json={"charges": charges},
            timeout=30,
        )
        r.raise_for_status()
        return r.json()["transactionIds"]
