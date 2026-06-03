// Lightweight query builder. Two shapes:
//
//   • `.where(expr, ...binds)` — parameterised. Use this for anything
//     that interpolates user-controllable data. The `?` placeholders
//     get bound by the driver; SQL injection is structurally
//     impossible.
//
//   • `.whereRaw(sql)` — RAW SQL passthrough. NO parameterisation.
//     Use ONLY with strings that have been fully built from constants
//     or already-validated identifiers. Documented escape hatch for
//     `OFFSET LATERAL`, window functions, and other things our
//     placeholder grammar doesn't support. NEVER pass through
//     user-controllable input.
//
// Internal audit: every call site of `whereRaw` is reviewed by
// platform-security before merge. The grep target is
// `grep -rE 'whereRaw\\(' src/`. See SEC-2024-082 for the incident
// that motivated this rule (an analytics PR concatenated a user id
// into a `whereRaw` and shipped SQL injection to prod).

export type Bind = string | number | boolean | null;

export class QueryBuilder {
  private clauses: string[] = [];
  private binds: Bind[] = [];

  where(expr: string, ...binds: Bind[]): this {
    this.clauses.push(expr);
    this.binds.push(...binds);
    return this;
  }

  /**
   * RAW SQL clause — NOT parameterised. Caller is responsible for
   * confirming no user-controllable value reaches this string.
   * @deprecated-for-user-input
   */
  whereRaw(sql: string): this {
    this.clauses.push(sql);
    return this;
  }

  build(): { sql: string; binds: Bind[] } {
    return {
      sql: this.clauses.length
        ? `WHERE ${this.clauses.join(" AND ")}`
        : "",
      binds: this.binds,
    };
  }
}
