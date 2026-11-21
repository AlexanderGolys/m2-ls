# Reproducing the Core formatting statistics

These scripts are throwaway analysis code. They are not part of m2-ls.

```sh
git clone --depth 1 --filter=blob:none --sparse -b development https://github.com/Macaulay2/M2.git m2src
git -C m2src sparse-checkout set M2/Macaulay2/m2
mkdir tools && cp mask.py tok.py stats.py report.py layout.py rep2.py ifs.py indentmodel.py tools/
cp files.txt .            # the 94 analysed files
python3 -I tools/stats.py  m2src/M2/Macaulay2/m2 per.pkl    && python3 -I tools/report.py per.pkl
python3 -I tools/layout.py m2src/M2/Macaulay2/m2 layout.pkl && python3 -I tools/rep2.py layout.pkl
python3 -I tools/ifs.py    m2src/M2/Macaulay2/m2 files.txt
python3 -I tools/indentmodel.py "(4,5)"    # per-file width; "(4,)" or "(5,)" for a fixed width
```

* `mask.py` blanks out strings and comments.
* `tok.py` is the tokenizer.
* `stats.py` and `report.py` measure line length, tabs, indent steps, blank lines and comments.
* `layout.py` and `rep2.py` measure operator, comma and bracket spacing and where closing brackets go.
* `ifs.py` measures `if`/`then`/`else`, `do` and `->` layout.
* `indentmodel.py` scores the indentation model of §4.1.
