/** A turn that ended on an API error (Claude's StopFailure): the words any surface shows instead
 *  of "Working…" once the turn is over. Kinds are Claude's own (2.1.310); an unknown one still
 *  reads as a failure. The Hive's notices carry Claude's words and advice from Rust instead
 *  (hive/turn_failure.rs); this is the generic line for every tab. */

const KIND_TEXT: Record<string, string> = {
  authentication_failed: "its Claude account isn't logged in: run /login",
  oauth_org_not_allowed: "its organization doesn't allow this login",
  account_on_hold: 'its Claude account is on hold',
  verification_required: 'its organization needs verifying',
  cloud_credential_error: 'its cloud credentials are missing or expired',
  billing_error: 'its account is out of credit or over its plan',
  rate_limit: 'it hit a usage limit',
  overloaded: "Anthropic's API was overloaded",
  server_error: "Anthropic's API was unavailable",
  model_not_found: "its model isn't available to this account",
  max_output_tokens: 'its reply ran past the output limit',
  invalid_request: 'Claude refused the request',
};

export function turnFailureText(kind: string): string {
  return `The last turn failed: ${KIND_TEXT[kind] ?? 'its terminal says why'}.`;
}

/** The Hive's notice for an agent whose turn failed (hive/turn_failure.rs `TurnFailure`). */
export function hiveFailureNotice(f: { text: string; advice: string }): string {
  return `This agent's turn failed: ${f.text.replace(/[.\s]+$/, '')}. ${f.advice}`;
}
