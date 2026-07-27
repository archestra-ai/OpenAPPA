.PHONY: all clean watch arxiv

LATEXMK := $(shell command -v latexmk 2>/dev/null)

ARXIV_SRC = appa.tex appa.bbl appa.bib $(wildcard sections/*.tex) \
            figures/VisAbstract.png

all: appa.pdf

appa.pdf appa.bbl: appa.tex appa.bib $(wildcard sections/*.tex)
ifdef LATEXMK
	latexmk -pdf -interaction=nonstopmode appa.tex
else
	pdflatex -interaction=nonstopmode appa.tex
	bibtex appa
	pdflatex -interaction=nonstopmode appa.tex
	pdflatex -interaction=nonstopmode appa.tex
endif

# arXiv does not run BibTeX: the generated .bbl must ship with the source.
arxiv: appa-arxiv.tar.gz

# COPYFILE_DISABLE keeps macOS tar from emitting AppleDouble ._* sidecars.
appa-arxiv.tar.gz: appa.pdf appa.bbl
	COPYFILE_DISABLE=1 tar czf $@ $(ARXIV_SRC)

watch:
	latexmk -pdf -pvc -interaction=nonstopmode appa.tex

clean:
	rm -f appa.pdf appa.aux appa.bbl appa.blg appa.log appa.out appa.fls \
	      appa.fdb_latexmk appa.synctex.gz appa.toc appa-arxiv.tar.gz
