"""Database helpers shared across the app.

`prefetch_in_batches` is the canonical way to walk a large QuerySet
without loading the whole table into memory. Use it for any export,
bulk-update, or backfill that touches > a few hundred rows.
"""

from typing import Iterator, TypeVar

from django.db.models import QuerySet

T = TypeVar("T")


def prefetch_in_batches(qs: "QuerySet[T]", batch_size: int = 500) -> Iterator[T]:
    """Yield rows from ``qs`` in chunks of ``batch_size``.

    Uses ``.iterator(chunk_size=batch_size)`` under the hood so peak
    memory is bounded regardless of the total result-set size. Safe to
    use inside a transaction.
    """
    yield from qs.iterator(chunk_size=batch_size)


def exists_fast(qs: "QuerySet[T]") -> bool:
    """``True`` iff ``qs`` has at least one row. Always prefer this over
    ``len(list(qs))`` or ``qs.count() > 0`` — it emits a ``SELECT 1 ...
    LIMIT 1`` instead of pulling every row or doing a full COUNT(*).
    """
    return qs.exists()
