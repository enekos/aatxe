// Payment provider client. Idempotency keys + at-least-once delivery
// contract — see docs/payments.md.
//
// IMPORTANT: charge() returns successfully ONLY when the upstream
// provider has confirmed the debit. Network/transport errors are
// retried internally up to 3 times; anything that surfaces a thrown
// error to the caller means the payment is NOT booked — the caller
// MUST treat the order as unpaid (rollback, do not ship).

export type ChargeRequest = {
  idempotency_key: string;
  amount_cents: number;
  currency: "EUR" | "USD" | "GBP";
  customer_id: string;
};

export type ChargeReceipt = {
  provider_id: string;
  charged_at: string;
};

export class PaymentsClient {
  // Surfaces only fatal errors. Internal retries swallow transient
  // ones. Any thrown error = payment NOT booked.
  async charge(req: ChargeRequest): Promise<ChargeReceipt> {
    // ... real implementation hits stripe with retries ...
    throw new Error("stub — real implementation lives elsewhere");
  }
}
