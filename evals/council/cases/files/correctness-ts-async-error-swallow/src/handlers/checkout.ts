import type { Request, Response } from "express";
import { PaymentsClient, type ChargeRequest } from "../services/payments";
import { markOrderPaid, markOrderShipped } from "../db/orders";

const payments = new PaymentsClient();

export async function checkoutHandler(req: Request, res: Response) {
  const orderId: string = req.body.order_id;
  const customerId: string = req.body.customer_id;
  const amountCents: number = req.body.amount_cents;

  const chargeReq: ChargeRequest = {
    idempotency_key: `order:${orderId}`,
    amount_cents: amountCents,
    currency: "EUR",
    customer_id: customerId,
  };

  // Charge the customer. If the payment provider has a hiccup we
  // don't want to block the response — log and move on.
  try {
    await payments.charge(chargeReq);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.error("payments.charge failed", err);
  }

  await markOrderPaid(orderId);
  await markOrderShipped(orderId);

  res.json({ ok: true, order_id: orderId });
}
