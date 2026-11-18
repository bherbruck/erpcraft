// Tab-separated text as Excel and Google Sheets put it on the clipboard.
//
// A cell holding a tab, a line break or a double quote is wrapped in double
// quotes, with each quote inside doubled. Rows end with CRLF (Excel) or LF
// (Sheets, Numbers), and Excel ends the last row with one too: that final
// line break does not start another row.

/** Turn clipboard text into rows of cells. */
export function parseTsv(text: string): string[][] {
  if (text === "") return [];
  const rows: string[][] = [];
  let row: string[] = [];
  let cell = "";
  let i = 0;
  const n = text.length;
  // At the start of a cell: is it quoted?
  let atCellStart = true;

  while (i < n) {
    const c = text[i];
    if (atCellStart && c === '"') {
      // A quoted cell: read up to the closing quote, "" being one quote.
      i += 1;
      let closed = false;
      while (i < n) {
        const d = text[i];
        if (d === '"') {
          if (text[i + 1] === '"') {
            cell += '"';
            i += 2;
            continue;
          }
          i += 1;
          closed = true;
          break;
        }
        cell += d;
        i += 1;
      }
      if (!closed) {
        // Never closed: the quote was text, as Excel reads it.
        cell = `"${cell}`;
      }
      atCellStart = false;
      continue;
    }
    atCellStart = false;
    if (c === "\t") {
      row.push(cell);
      cell = "";
      atCellStart = true;
      i += 1;
    } else if (c === "\r" || c === "\n") {
      row.push(cell);
      rows.push(row);
      row = [];
      cell = "";
      atCellStart = true;
      i += c === "\r" && text[i + 1] === "\n" ? 2 : 1;
    } else {
      cell += c;
      i += 1;
    }
  }
  // Text that does not end with a line break has one more row; the line
  // break Excel puts after the last row starts none.
  if (!atCellStart || row.length > 0) {
    row.push(cell);
    rows.push(row);
  }
  return rows;
}

function quoteCell(cell: string): string {
  if (/[\t\r\n"]/.test(cell)) return `"${cell.replaceAll('"', '""')}"`;
  return cell;
}

/** Turn rows of cells into clipboard text that Excel and Sheets paste cell by cell. */
export function toTsv(rows: readonly (readonly string[])[]): string {
  return rows.map((row) => row.map(quoteCell).join("\t")).join("\r\n");
}

/** An HTML table of the same cells, which spreadsheets also accept. */
export function toHtmlTable(rows: readonly (readonly string[])[]): string {
  const escape = (s: string) =>
    s.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll("\n", "<br>");
  const body = rows.map((row) => `<tr>${row.map((cell) => `<td>${escape(cell)}</td>`).join("")}</tr>`).join("");
  return `<table>${body}</table>`;
}
