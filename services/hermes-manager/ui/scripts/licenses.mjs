// Preserve the notices of every production dependency in the runtime image.
import { readFileSync, readdirSync, writeFileSync } from "node:fs";
const lock = JSON.parse(readFileSync("package-lock.json", "utf8"));
const notices = [];
for (const [path, item] of Object.entries(lock.packages)) {
  if (!path || item.dev) continue;
  const licenses = readdirSync(path).filter((name) =>
    /^licen[cs]e(?:\.|$)/i.test(name),
  );
  if (!licenses.length) {
    if (
      path === "node_modules/react-remove-scroll-bar" &&
      item.license === "MIT"
    ) {
      notices.push(
        `${path}@${item.version} — MIT\n${readFileSync(`${path}/README.md`, "utf8")}`,
      );
      continue;
    }
    throw new Error(`Missing upstream license notice: ${path}`);
  }
  notices.push(
    `${path}@${item.version}\n${licenses.map((name) => readFileSync(`${path}/${name}`, "utf8")).join("\n")}`,
  );
}
writeFileSync(
  "dist/THIRD_PARTY_NOTICES.txt",
  notices.join("\n\n--------------------\n\n"),
);
