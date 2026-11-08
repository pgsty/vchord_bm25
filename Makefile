export PG_CONFIG ?= pg_config
PKGLIBDIR := $(shell $(PG_CONFIG) --pkglibdir)
SHAREDIR := $(shell $(PG_CONFIG) --sharedir)
CP_R ?= cp -r
MKDIR_P ?= mkdir -p
DISTVERSION ?= $(shell sed -n "s/^default_version = '\([^']*\)'/\1/p" vchord_bm25.control)
DISTFILE ?= vchord_bm25-$(DISTVERSION).zip

.PHONY: all build clippy install uninstall dist

all: build

build:
	cargo run -p xtask -- build

clippy:
	cargo run -p xtask -- clippy

install:
	$(MKDIR_P) $(DESTDIR)$(PKGLIBDIR) && \
	$(CP_R) ./build/pkglibdir/. $(DESTDIR)$(PKGLIBDIR) && \
	$(MKDIR_P) $(DESTDIR)$(SHAREDIR) && \
	$(CP_R) ./build/sharedir/. $(DESTDIR)$(SHAREDIR)

uninstall:
	$(RM) $(wildcard $(DESTDIR)$(PKGLIBDIR)/vchord_bm25.*) && \
	$(RM) $(wildcard $(DESTDIR)$(SHAREDIR)/extension/vchord_bm25.*) && \
	$(RM) $(wildcard $(DESTDIR)$(SHAREDIR)/extension/vchord_bm25--*.sql)

dist:
	@test -n "$(DISTVERSION)" || (echo "could not read vchord_bm25.control version" >&2; exit 1)
	sed -e "s/@DISTVERSION@/$(DISTVERSION)/g" META.json.in > META.json
	python3 -c 'import json; json.load(open("META.json")); assert json.load(open("META.json"))["version"] == "$(DISTVERSION)"'
	git archive --format zip --prefix "vchord_bm25-$(DISTVERSION)/" --add-file META.json -o "$(DISTFILE)" HEAD
	$(RM) META.json
