import type { Language } from "./preview";

export type TokenKind = "comment" | "string" | "number" | "keyword" | "tag" | "attr" | "plain";

export interface Token {
  kind: TokenKind;
  text: string;
}

type Rule = [TokenKind, RegExp];

const STRING = /"(?:\\.|[^"\\\n])*"?|'(?:\\.|[^'\\\n])*'?/y;
const NUMBER = /-?\b\d+(?:\.\d+)?(?:[eE][+-]?\d+)?\b/y;
const JS_KEYWORDS =
  /\b(?:const|let|var|function|return|if|else|for|while|do|switch|case|break|continue|new|class|extends|import|export|from|default|async|await|try|catch|finally|throw|typeof|instanceof|in|of|this|null|undefined|true|false|yield|delete|void)\b/y;

const RULES: Record<Exclude<Language, "plain">, Rule[]> = {
  json: [
    ["attr", /"(?:\\.|[^"\\\n])*"(?=\s*:)/y],
    ["string", /"(?:\\.|[^"\\\n])*"?/y],
    ["number", NUMBER],
    ["keyword", /\b(?:true|false|null)\b/y],
  ],
  js: [
    ["comment", /\/\/[^\n]*|\/\*[\s\S]*?(?:\*\/|$)/y],
    ["string", /`(?:\\[\s\S]|[^`\\])*`?/y],
    ["string", STRING],
    ["number", NUMBER],
    ["keyword", JS_KEYWORDS],
  ],
  css: [
    ["comment", /\/\*[\s\S]*?(?:\*\/|$)/y],
    ["string", STRING],
    ["keyword", /@[\w-]+|!important\b/y],
    ["attr", /[\w-]+(?=\s*:[^:])/y],
    ["number", /#[0-9a-fA-F]{3,8}\b|-?\b\d+(?:\.\d+)?(?:px|em|rem|%|vh|vw|s|ms|deg|fr)?\b/y],
  ],
  xml: [
    ["comment", /<!--[\s\S]*?(?:-->|$)/y],
    ["tag", /<\/?[\w:.-]+|\/?>|<!DOCTYPE[^>]*>|<\?[\s\S]*?\?>/y],
    ["attr", /[\w:.-]+(?==)/y],
    ["string", STRING],
  ],
};

/** Splits text into colored pieces, one list per line. Unknown text stays plain. */
export function highlight(text: string, language: Language): Token[][] {
  const tokens: Token[] = [];
  const rules = language === "plain" ? [] : RULES[language];
  let plain = "";
  let at = 0;
  while (at < text.length) {
    let matched: Token | null = null;
    for (const [kind, pattern] of rules) {
      pattern.lastIndex = at;
      const found = pattern.exec(text);
      if (found && found[0].length > 0) {
        matched = { kind, text: found[0] };
        break;
      }
    }
    if (matched) {
      if (plain) tokens.push({ kind: "plain", text: plain });
      plain = "";
      tokens.push(matched);
      at += matched.text.length;
    } else {
      plain += text[at];
      at++;
    }
  }
  if (plain) tokens.push({ kind: "plain", text: plain });
  return toLines(tokens);
}

function toLines(tokens: Token[]): Token[][] {
  const lines: Token[][] = [[]];
  for (const token of tokens) {
    const parts = token.text.split("\n");
    parts.forEach((part, i) => {
      if (i > 0) lines.push([]);
      if (part) lines[lines.length - 1].push({ kind: token.kind, text: part });
    });
  }
  if (lines.length > 1 && lines[lines.length - 1].length === 0) lines.pop();
  return lines;
}
