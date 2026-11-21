// The browser's copy of the success condition, used only to know when to stop
// recording. The daemon's rules (pow/sm64pow/src/rules.rs) are the authority:
// if you change Rules::is_success there, change isSuccess here and add the
// new rules name.
export const KNOWN_RULES = {
  'any-star': (state) => state.numStars > 0, // JS GameState field names are camelCase
  'n-stars': null, // needs N: see rules.rs `Stars`
};

export function successFor(rulesName) {
  return KNOWN_RULES[rulesName] || null;
}
