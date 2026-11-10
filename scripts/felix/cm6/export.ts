// Generate key-sequence cases from a pinned helix-cm6 checkout. The cases are
// produced at run time and never committed to Red.
//
//   bun scripts/felix/cm6/export.ts <cm6-dir> hand|fuzz:<seed>[:<count>]|md|code > cases.json
import { resolve } from "node:path";

const [dir, suite] = process.argv.slice(2);
if (!dir || !suite) {
  console.error("usage: export.ts <cm6-dir> hand|fuzz:<seed>[:<count>]|md|code");
  process.exit(2);
}

const suites = await import(resolve(dir, "test/oracle/suites.ts"));
const fuzz = await import(resolve(dir, "test/oracle/fuzz.ts"));

type Case = { id: string; in: string; keys: string; lang?: string };
let cases: Case[];
const [name, seed, count] = suite.split(":");
switch (name) {
  case "hand":
    cases = suites.handSuite();
    break;
  case "fuzz":
    cases = fuzz.fuzzSuite(Number(count ?? 3000), Number(seed ?? 1));
    break;
  case "md":
    cases = suites.markdownSuite();
    break;
  case "code":
    cases = suites.codeSuite();
    break;
  default:
    console.error(`unknown suite ${suite}`);
    process.exit(2);
}

process.stdout.write(JSON.stringify(cases.map(({ id, in: input, keys, lang }) => ({ id, in: input, keys, lang }))));
