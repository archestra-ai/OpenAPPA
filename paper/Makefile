.PHONY: all clean watch

all: appa.pdf

appa.pdf: appa.tex appa.bib $(wildcard sections/*.tex)
	latexmk -pdf -interaction=nonstopmode appa.tex

watch:
	latexmk -pdf -pvc -interaction=nonstopmode appa.tex

clean:
	latexmk -C appa.tex
