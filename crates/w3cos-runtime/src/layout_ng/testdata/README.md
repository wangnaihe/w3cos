# layout_ng test data

- `generated-blocks-chrome.json`, `generated-inline-chrome.json`: Chrome's
  rects (1/64 px) for the generated corpora, written by
  `tools/layout-ng/chrome_oracle.py --write-golden`.
- `Ahem.ttf`: the W3C Ahem test font (version 1.50), copied byte-for-byte
  from web-platform-tests `fonts/Ahem.ttf` (the WPT repository is
  BSD-3-Clause licensed). The font's own license, verbatim from its `name`
  table (copyright, ID 0; license URL, ID 14):

  > The Ahem font belongs to the public domain. In jurisdictions that do not
  > recognize public domain ownership of these files, the following Creative
  > Commons Zero declaration applies:
  > http://labs.creativecommons.org/licenses/zero-waive/1.0/us/legalcode

  License URL: http://dev.w3.org/CSS/fonts/ahem/COPYING
