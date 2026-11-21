export PG_CONFIG ?= pg_config
PKGLIBDIR := $(shell $(PG_CONFIG) --pkglibdir)
SHAREDIR := $(shell $(PG_CONFIG) --sharedir)
INSTALL ?= install
DISTVERSION ?= $(shell sed -n "s/^default_version = '\([^']*\)'/\1/p" vchord_bm25.control)
DISTFILE ?= vchord_bm25-$(DISTVERSION).zip

.PHONY: all build clippy install uninstall dist

all: build

build:
	cargo run --locked -p xtask -- build

clippy:
	cargo run --locked -p xtask -- clippy

install:
	$(INSTALL) -d $(DESTDIR)$(PKGLIBDIR) $(DESTDIR)$(SHAREDIR)/extension && \
	$(INSTALL) -m 755 ./build/pkglibdir/* $(DESTDIR)$(PKGLIBDIR)/ && \
	$(INSTALL) -m 644 ./build/sharedir/extension/* $(DESTDIR)$(SHAREDIR)/extension/

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
