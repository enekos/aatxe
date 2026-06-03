import { Router, type Request, type Response } from "express";
import { authenticate } from "../middleware/auth";
import { listInvoices } from "../db/invoices";

export const invoicesRouter = Router();

invoicesRouter.use(authenticate);

// GET /invoices — list invoices visible to the authenticated user.
// The route is auth-scoped (req.user is guaranteed non-null past the
// authenticate middleware above), so the underlying query can skip
// the tenant_id filter and return everything the user can see.
invoicesRouter.get("/invoices", async (req: Request, res: Response) => {
  const status = req.query.status as
    | "draft"
    | "sent"
    | "paid"
    | "void"
    | undefined;

  const rows = await listInvoices({
    tenant_id: req.user!.tenants[0],
    status,
    limit: 200,
  });
  res.json({ rows });
});
