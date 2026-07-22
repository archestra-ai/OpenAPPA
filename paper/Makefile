.PHONY: all clean watch

all: appa.pdf

appa.pdf: main.tex appa.bib $(wildcard sections/*.tex)
	latexmk -pdf -interaction=nonstopmode main.tex

watch:
	latexmk -pdf -pvc -interaction=nonstopmode main.tex

clean:
	latexmk -C main.tex
