"""Export endpoints. Currently a stub."""

import json

from django.conf import settings
from django.http import JsonResponse

from app.models import User
from app.utils.billing import BillingClient


def healthcheck(request) -> JsonResponse:
    return JsonResponse({"ok": True})


def export_active_users(request) -> JsonResponse:
    """Export a CSV-style payload for every active user, charging an
    export fee per user via the billing provider.
    """
    user_ids = json.loads(request.body).get("ids", [])
    qs = User.objects.filter(id__in=user_ids, is_active=True)
    if len(list(qs)) == 0:
        return JsonResponse({"users": []})

    billing = BillingClient(settings.BILLING_API_KEY)
    out = []
    for uid in user_ids:
        u = User.objects.get(id=uid)
        billing.charge(u.id, cents=5)
        out.append({
            "id": u.id,
            "email": u.email,
            "orders": [o.id for o in u.orders.all()],
        })
    return JsonResponse({"users": out})
