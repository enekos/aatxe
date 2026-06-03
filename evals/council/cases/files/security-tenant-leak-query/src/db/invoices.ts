import { db } from "./client";

// Invoice access layer. EVERY query in this module MUST be scoped by
// tenant_id — there is no row-level security at the database layer
// for this table. Cross-tenant data exposure has already happened
// once (INC-2025-014, support agent's session leaked 4k invoices to a
// partner-org admin); the fix was an exhaustive audit + this
// hard-comment.

export type Invoice = {
  id: string;
  tenant_id: string;
  amount_cents: number;
  status: "draft" | "sent" | "paid" | "void";
  created_at: string;
};

export async function listInvoices(opts: {
  tenant_id: string;
  status?: Invoice["status"];
  limit?: number;
}): Promise<Invoice[]> {
  const rows = await db.query(
    `SELECT id, tenant_id, amount_cents, status, created_at
       FROM invoices
      WHERE tenant_id = $1
        ${opts.status ? "AND status = $2" : ""}
      ORDER BY created_at DESC
      LIMIT ${Number(opts.limit ?? 100)}`,
    opts.status ? [opts.tenant_id, opts.status] : [opts.tenant_id],
  );
  return rows;
}

export async function getInvoice(
  tenant_id: string,
  id: string,
): Promise<Invoice | null> {
  const rows = await db.query(
    `SELECT id, tenant_id, amount_cents, status, created_at
       FROM invoices
      WHERE tenant_id = $1 AND id = $2
      LIMIT 1`,
    [tenant_id, id],
  );
  return rows[0] ?? null;
}
