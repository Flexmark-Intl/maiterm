import { describe, expect, it } from 'vitest';
import { hiveFailureNotice, turnFailureText } from './turnFailure';

describe('a failed turn', () => {
  it('says the turn is over and why, for any kind', () => {
    expect(turnFailureText('authentication_failed')).toBe("The last turn failed: its Claude account isn't logged in: run /login.");
    expect(turnFailureText('something_new')).toBe('The last turn failed: its terminal says why.');
  });

  it("reads Claude's words then maiTerm's advice on a Hive card", () => {
    expect(hiveFailureNotice({ text: 'Login expired.', advice: "Its Claude account isn't logged in: run /login in a Claude tab of that account." }))
      .toBe("This agent's turn failed: Login expired. Its Claude account isn't logged in: run /login in a Claude tab of that account.");
  });
});
