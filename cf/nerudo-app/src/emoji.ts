// Emoji for the composer's :shortcode: completion and the reaction picker. The Worker
// keeps its own table and normalizes whatever is sent; this list is for typing.

export const QUICK = ["👍", "🎉", "🚀", "👀", "❤️", "😄", "😕", "👎"];

export const SHORTCODES: [string, string][] = [
  ["+1", "👍"], ["-1", "👎"], ["ok_hand", "👌"], ["clap", "👏"], ["raised_hands", "🙌"], ["pray", "🙏"],
  ["muscle", "💪"], ["wave", "👋"], ["eyes", "👀"], ["brain", "🧠"], ["smile", "😄"], ["grin", "😁"],
  ["joy", "😂"], ["wink", "😉"], ["thinking", "🤔"], ["confused", "😕"], ["sweat_smile", "😅"],
  ["sunglasses", "😎"], ["nerd_face", "🤓"], ["facepalm", "🤦"], ["shrug", "🤷"], ["skull", "💀"],
  ["robot", "🤖"], ["heart", "❤️"], ["100", "💯"], ["fire", "🔥"], ["sparkles", "✨"], ["star", "⭐"],
  ["zap", "⚡"], ["boom", "💥"], ["tada", "🎉"], ["trophy", "🏆"], ["rocket", "🚀"], ["airplane", "✈️"],
  ["construction", "🚧"], ["rotating_light", "🚨"], ["warning", "⚠️"], ["stop_sign", "🛑"], ["x", "❌"],
  ["white_check_mark", "✅"], ["question", "❓"], ["bulb", "💡"], ["memo", "📝"], ["books", "📚"],
  ["link", "🔗"], ["pushpin", "📌"], ["package", "📦"], ["lock", "🔒"], ["key", "🔑"], ["shield", "🛡️"],
  ["hammer_and_wrench", "🛠️"], ["gear", "⚙️"], ["test_tube", "🧪"], ["mag", "🔍"], ["computer", "💻"],
  ["bell", "🔔"], ["speech_balloon", "💬"], ["hourglass", "⌛"], ["recycle", "♻️"], ["broom", "🧹"],
  ["bug", "🐛"], ["snail", "🐌"], ["turtle", "🐢"], ["crab", "🦀"], ["seedling", "🌱"], ["rainbow", "🌈"],
  ["coffee", "☕"], ["pizza", "🍕"], ["checkered_flag", "🏁"], ["dart", "🎯"], ["art", "🎨"],
];

export const PICKER = [
  "👍", "👎", "🎉", "🚀", "👀", "❤️", "😄", "😕", "🙏", "👏", "🔥", "✨", "💯", "✅", "❌", "⚠️",
  "🤔", "😅", "🤖", "🧠", "💡", "🐛", "🛠️", "🧪", "🔒", "🏁", "🛬", "🛫", "📦", "🧹", "☕", "🌱",
];

const TABLE = new Map(SHORTCODES);

/** Text with known :shortcodes: turned into emoji. */
export function withEmoji(text: string): string {
  return text.replace(/:([a-z0-9_+-]+):/g, (m, name: string) => TABLE.get(name) ?? m);
}
