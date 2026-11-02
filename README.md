# vchord_bm25

[![Website: vchord.pgsty.com](https://img.shields.io/badge/Website-vchord.pgsty.com-slategray?style=flat&logo=cilium&logoColor=white)](https://vchord.pgsty.com/)
[![Docs: vchord_bm25](https://img.shields.io/badge/Docs-vchord__bm25-slategray?style=flat)](https://vchord.pgsty.com/bm25/)
[![PGEXT: vchord_bm25](https://img.shields.io/badge/PGEXT-vchord__bm25-%233E668F?style=flat&logo=postgresql&logoColor=white&labelColor=3E668F)](https://pgext.cloud/ext/vchord_bm25)
[![Pigsty: vchord_bm25](https://img.shields.io/badge/Pigsty-vchord__bm25-slategray?style=flat)](https://pigsty.io/ext/e/vchord_bm25)
[![Version: v0.3.0](https://img.shields.io/badge/Version-v0.3.0-slategray?style=flat)](https://github.com/pgsty/vchord_bm25/releases)
[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-slategray?style=flat&logo=gnu&logoColor=white)](https://github.com/pgsty/vchord_bm25/blob/main/LICENSE)
[![GitHub Stars](https://img.shields.io/github/stars/pgsty/vchord_bm25?style=flat&logo=github&logoColor=black&color=slategray)](https://star-history.com/#pgsty/vchord_bm25&Date)
[![PostgreSQL: 14-18](https://img.shields.io/badge/PostgreSQL-14--18-%233E668F?style=flat&logo=postgresql&logoColor=white&labelColor=3E668F)](https://pgext.cloud/ext/vchord_bm25)

**`vchord_bm25`** is a PostgreSQL extension for native BM25 full-text ranking powered by the Block-WeakAnd (WAND) algorithm, designed to work alongside [`pg_tokenizer`](https://github.com/pgsty/pg_tokenizer) and [`vchord`](https://github.com/pgsty/vchord).

[Website](https://vchord.pgsty.com/) | [BM25 Docs](https://vchord.pgsty.com/bm25/) | [Tokenizer Docs](https://vchord.pgsty.com/bm25/tokenizer/overview/) | [vchord](https://github.com/pgsty/vchord) | [pg_tokenizer](https://github.com/pgsty/pg_tokenizer) | [PGEXT](https://pgext.cloud/ext/vchord_bm25) | [Pigsty](https://pigsty.io/ext/e/vchord_bm25) ([中文](https://pigsty.cc/ext/e/vchord_bm25)) | [Releases](https://github.com/pgsty/vchord_bm25/releases) | [Issues](https://github.com/pgsty/vchord_bm25/issues)

---

## Vchord Suite

`vchord_bm25` is part of the **Vchord** extension family maintained by [PGSTY](https://pgsty.com):

- **[`vchord`](https://github.com/pgsty/vchord)** ([`pgsty/vchord`](https://github.com/pgsty/vchord) · [PGEXT](https://pgext.cloud/ext/vchord) · [Pigsty](https://pigsty.io/ext/e/vchord)): Scalable, high-performance, and disk-friendly vector similarity search (`vchordrq` & `vchordg`).
- **[`vchord_bm25`](https://github.com/pgsty/vchord_bm25)** ([`pgsty/vchord_bm25`](https://github.com/pgsty/vchord_bm25) · [PGEXT](https://pgext.cloud/ext/vchord_bm25) · [Pigsty](https://pigsty.io/ext/e/vchord_bm25)): Native BM25 full-text ranking index (`Block-WeakAnd`).
- **[`pg_tokenizer`](https://github.com/pgsty/pg_tokenizer)** ([`pgsty/pg_tokenizer`](https://github.com/pgsty/pg_tokenizer) · [PGEXT](https://pgext.cloud/ext/pg_tokenizer) · [Pigsty](https://pigsty.io/ext/e/pg_tokenizer)): Standalone multilingual text analyzer and vocabulary tokenizer.

---

## Features

- **Block-WeakAnd BM25 Index**: Native PostgreSQL index access method (`USING bm25`) with block-max pruning for fast top-K lexical ranking without scanning every matching row.
- **Sparse `bm25vector` Data Type**: Represents tokenized documents as compact sparse vectors of vocabulary IDs and term frequencies, scored via the `<&>` operator.
- **Customizable Tokenization**: Pairs with [`pg_tokenizer`](https://github.com/pgsty/pg_tokenizer) to support pre-trained models (`bert_base_uncased`, `llmlingua2`, `gemma2b`), corpus-trained custom models, and multilingual segmentation (English, Chinese `jieba`, Japanese `lindera`).
- **Native Hybrid Search**: Combines seamlessly with [`vchord`](https://github.com/pgsty/vchord) dense and multi-vector indexes in SQL using Reciprocal Rank Fusion (RRF).

---

## Quick Start

For full setup details (Debian packages, source builds, and configuration), see the [BM25 Installation Guide](https://vchord.pgsty.com/bm25/getting-started/installation/).

### 1. Install via Native Linux Packages ([`pig`](https://pig.pgsty.com/))

On EL 8/9/10, Debian 12/13, and Ubuntu 22.04/24.04 (PostgreSQL 14–18), install pre-built native packages from the [Pigsty repository](https://pigsty.io/ext/e/vchord_bm25) (see package matrix on [pgext.cloud](https://pgext.cloud/ext/vchord_bm25)):

```bash
curl -fsSL https://repo.pigsty.io/pig | bash
pig repo add pgsql -u
pig ext add vchord_bm25 pg_tokenizer
```

### 2. Run with Docker

You can also start a ready-to-use container with the [`tensorchord/vchord-suite`](https://vchord.pgsty.com/docs/getting-started/vectorchord-suite/) image, which bundles PostgreSQL, [`vchord`](https://github.com/pgsty/vchord), [`vchord_bm25`](https://github.com/pgsty/vchord_bm25), and [`pg_tokenizer`](https://github.com/pgsty/pg_tokenizer):

```bash
docker run \
  --name vchord-suite \
  -e POSTGRES_PASSWORD=postgres \
  -p 5432:5432 \
  -d tensorchord/vchord-suite:pg18-latest
  # Or from GHCR: ghcr.io/tensorchord/vchord-suite:pg18-latest
```

Connect to the database using `psql`:

```bash
psql -h localhost -p 5432 -U postgres
```

### 3. Enable Extensions

Enable `pg_tokenizer` and `vchord_bm25`, and include `bm25_catalog` and `tokenizer_catalog` in your `search_path`:

```sql
CREATE EXTENSION IF NOT EXISTS pg_tokenizer CASCADE;
CREATE EXTENSION IF NOT EXISTS vchord_bm25 CASCADE;

SET search_path TO "$user", public, bm25_catalog, tokenizer_catalog;
```

---

## Usage

A typical BM25 workflow consists of three parts: a **tokenizer** that converts text into token IDs, a **`bm25vector`** column that stores token IDs and term frequencies, and a **`bm25` index** that maintains global document frequencies and accelerates top-K ranking.

> [!NOTE]
> Tokenization is provided by [`pg_tokenizer`](https://github.com/pgsty/pg_tokenizer). See the [Tokenizer Documentation](https://vchord.pgsty.com/bm25/tokenizer/overview/) for model configuration details.

### 1. Create a Tokenizer

```sql
-- Create a tokenizer using a pre-trained BERT vocabulary
SELECT create_tokenizer('bert', $$
model = "bert_base_uncased"
$$);

-- Tokenize sample text into a bm25vector
SELECT tokenize('A quick brown fox jumps over the lazy dog.', 'bert')::bm25vector;
-- Output: {1012:1, 1037:1, 1996:1, 2058:1, 2829:1, 3899:1, 4248:1, 4419:1, 13971:1, 14523:1}
```

### 2. Populate Documents and Build a `bm25` Index

Because BM25 scoring relies on global document frequency statistics across the corpus, create a `bm25` index on the `bm25vector` column before querying with `<&>`:

```sql
CREATE TABLE documents (
    id SERIAL PRIMARY KEY,
    passage TEXT,
    embedding bm25vector
);

INSERT INTO documents (passage) VALUES
('PostgreSQL is a powerful, open-source object-relational database system. It has over 15 years of active development.'),
('Full-text search is a technique for searching in plain-text documents or textual database fields. PostgreSQL supports this with tsvector.'),
('BM25 is a ranking function used by search engines to estimate the relevance of documents to a given search query.'),
('PostgreSQL provides many advanced features like full-text search, window functions, and more.'),
('Search and ranking in databases are important in building effective information retrieval systems.'),
('The BM25 ranking algorithm is derived from the probabilistic retrieval framework.'),
('Full-text search indexes documents to allow fast text queries. PostgreSQL supports this through its GIN and GiST indexes.'),
('The PostgreSQL community is active and regularly improves the database system.'),
('Relational databases such as PostgreSQL can handle both structured and unstructured data.'),
('Effective search ranking algorithms, such as BM25, improve search results by understanding relevance.');

UPDATE documents SET embedding = tokenize(passage, 'bert');

CREATE INDEX documents_embedding_bm25 ON documents USING bm25 (embedding bm25_ops);
```

### 3. Query with BM25 Ranking

Note that `vchord_bm25` returns **negative** BM25 scores (`<&>`) so that standard ascending `ORDER BY` returns the most relevant documents first:

```sql
SELECT id, passage, embedding <&> to_bm25query('documents_embedding_bm25', tokenize('PostgreSQL', 'bert')) AS rank
FROM documents
ORDER BY rank
LIMIT 10;
```

### More Examples

<details>
<summary>Using a custom model</summary>

#### Using a custom model

You can train a custom vocabulary model directly from your own text corpus:

```sql
CREATE TABLE documents (
    id SERIAL PRIMARY KEY,
    passage TEXT,
    embedding bm25vector
);

-- Create a text analyzer to normalize, filter stopwords, and stem tokens
SELECT create_text_analyzer('text_analyzer1', $$
pre_tokenizer = "unicode_segmentation"  # split texts according to Unicode Standard Annex #29
[[character_filters]]
to_lowercase = {}                       # convert all characters to lowercase
[[character_filters]]
unicode_normalization = "nfkd"          # normalize text to Unicode Normalization Form KD
[[token_filters]]
skip_non_alphanumeric = {}              # skip non-alphanumeric tokens
[[token_filters]]
stopwords = "nltk_english"              # remove stopwords using the NLTK dictionary
[[token_filters]]
stemmer = "english_porter2"             # stem tokens using the English Porter2 stemmer
$$);

-- Train a custom model from documents.passage and set up an automatic trigger
SELECT create_custom_model_tokenizer_and_trigger(
    tokenizer_name => 'tokenizer1',
    model_name => 'model1',
    text_analyzer_name => 'text_analyzer1',
    table_name => 'documents',
    source_column => 'passage',
    target_column => 'embedding'
);

INSERT INTO documents (passage) VALUES
('PostgreSQL is a powerful, open-source object-relational database system. It has over 15 years of active development.'),
('Full-text search is a technique for searching in plain-text documents or textual database fields. PostgreSQL supports this with tsvector.'),
('BM25 is a ranking function used by search engines to estimate the relevance of documents to a given search query.'),
('PostgreSQL provides many advanced features like full-text search, window functions, and more.'),
('Search and ranking in databases are important in building effective information retrieval systems.'),
('The BM25 ranking algorithm is derived from the probabilistic retrieval framework.'),
('Full-text search indexes documents to allow fast text queries. PostgreSQL supports this through its GIN and GiST indexes.'),
('The PostgreSQL community is active and regularly improves the database system.'),
('Relational databases such as PostgreSQL can handle both structured and unstructured data.'),
('Effective search ranking algorithms, such as BM25, improve search results by understanding relevance.');

CREATE INDEX documents_embedding_bm25 ON documents USING bm25 (embedding bm25_ops);

SELECT id, passage, embedding <&> to_bm25query('documents_embedding_bm25', tokenize('PostgreSQL', 'tokenizer1')) AS rank
FROM documents
ORDER BY rank
LIMIT 10;
```

</details>

<details>
<summary>Using jieba for Chinese text</summary>

#### Using jieba for Chinese text

For Chinese text, use the [`jieba`](https://github.com/messense/jieba-rs) pre-tokenizer to segment text into words and train a custom model:

```sql
CREATE TABLE documents (
    id SERIAL PRIMARY KEY,
    passage TEXT,
    embedding bm25vector
);

SELECT create_text_analyzer('text_analyzer1', $$
[pre_tokenizer.jieba]
$$);

SELECT create_custom_model_tokenizer_and_trigger(
    tokenizer_name => 'tokenizer1',
    model_name => 'model1',
    text_analyzer_name => 'text_analyzer1',
    table_name => 'documents',
    source_column => 'passage',
    target_column => 'embedding'
);

INSERT INTO documents (passage) VALUES
('红海早过了，船在印度洋面上开驶着，但是太阳依然不饶人地迟落早起，侵占去大部分的夜。'),
('夜仿佛纸浸了油变成半透明体；它给太阳拥抱住了，分不出身来，也许是给太阳陶醉了，所以夕照晚霞褪后的夜色也带着酡红。'),
('到红消醉醒，船舱里的睡人也一身腻汗地醒来，洗了澡赶到甲板上吹海风，又是一天开始。'),
('这是七月下旬，合中国旧历的三伏，一年最热的时候。在中国热得更比常年利害，事后大家都说是兵戈之象，因为这就是民国二十六年【一九三七年】。'),
('这条法国邮船白拉日隆子爵号（VicomtedeBragelonne）正向中国开来。'),
('早晨八点多钟，冲洗过的三等舱甲板湿意未干，但已坐满了人，法国人、德国流亡出来的犹太人、印度人、安南人，不用说还有中国人。'),
('海风里早含着燥热，胖人身体给炎风吹干了，上一层汗结的盐霜，仿佛刚在巴勒斯坦的死海里洗过澡。'),
('毕竟是清晨，人的兴致还没给太阳晒萎，烘懒，说话做事都很起劲。'),
('那几个新派到安南或中国租界当警察的法国人，正围了那年轻善撒娇的犹太女人在调情。'),
('俾斯麦曾说过，法国公使大使的特点，就是一句外国话不会讲；这几位警察并不懂德文，居然传情达意，引得犹太女人格格地笑，比他们的外交官强多了。'),
('这女人的漂亮丈夫，在旁顾而乐之，因为他几天来，香烟、啤酒、柠檬水沾光了不少。'),
('红海已过，不怕热极引火，所以等一会甲板上零星果皮、纸片、瓶塞之外，香烟头定又遍处皆是。'),
('法国人的思想是有名的清楚，他的文章也明白干净，但是他的做事，无不混乱、肮脏、喧哗，但看这船上的乱糟糟。'),
('这船，倚仗人的机巧，载满人的扰攘，寄满人的希望，热闹地行着，每分钟把沾污了人气的一小方小面，还给那无情、无尽、无际的大海。');

CREATE INDEX documents_embedding_bm25 ON documents USING bm25 (embedding bm25_ops);

SELECT id, passage, embedding <&> to_bm25query('documents_embedding_bm25', tokenize('人', 'tokenizer1')) AS rank
FROM documents
ORDER BY rank
LIMIT 10;
```

</details>

<details>
<summary>Using lindera for Japanese text</summary>

#### Using lindera for Japanese text

For Japanese text, use [`lindera`](https://github.com/lindera/lindera) with an IPADIC dictionary configuration:

> [!NOTE]
> `lindera` requires optional compile-time flags and is not enabled by default; build `pg_tokenizer` from source with `lindera` enabled if needed.

```sql
CREATE TABLE documents (
    id SERIAL PRIMARY KEY,
    passage TEXT,
    embedding bm25vector
);

SELECT create_lindera_model('lindera_ipadic', $$
[segmenter]
mode = "normal"
  [segmenter.dictionary]
  kind = "ipadic"
[[character_filters]]
kind = "unicode_normalize"
  [character_filters.args]
  kind = "nfkc"
[[character_filters]]
kind = "japanese_iteration_mark"
  [character_filters.args]
  normalize_kanji = true
  normalize_kana = true
[[character_filters]]
kind = "mapping"
[character_filters.args.mapping]
"リンデラ" = "Lindera"
[[token_filters]]
kind = "japanese_compound_word"
  [token_filters.args]
  kind = "ipadic"
  tags = [ "名詞,数", "名詞,接尾,助数詞" ]
  new_tag = "名詞,数"
[[token_filters]]
kind = "japanese_number"
  [token_filters.args]
  tags = [ "名詞,数" ]
[[token_filters]]
kind = "japanese_stop_tags"
  [token_filters.args]
  tags = [
  "接続詞",
  "助詞",
  "助詞,格助詞",
  "助詞,格助詞,一般",
  "助詞,格助詞,引用",
  "助詞,格助詞,連語",
  "助詞,係助詞",
  "助詞,副助詞",
  "助詞,間投助詞",
  "助詞,並立助詞",
  "助詞,終助詞",
  "助詞,副助詞／並立助詞／終助詞",
  "助詞,連体化",
  "助詞,副詞化",
  "助詞,特殊",
  "助動詞",
  "記号",
  "記号,一般",
  "記号,読点",
  "記号,句点",
  "記号,空白",
  "記号,括弧閉",
  "その他,間投",
  "フィラー",
  "非言語音"
]
[[token_filters]]
kind = "japanese_katakana_stem"
  [token_filters.args]
  min = 3
[[token_filters]]
kind = "remove_diacritical_mark"
  [token_filters.args]
  japanese = false
$$);

SELECT create_tokenizer('lindera_ipadic', $$
model = "lindera_ipadic"
$$);

INSERT INTO documents (passage) VALUES
('どこで生れたかとんと見当けんとうがつかぬ。'),
('何でも薄暗いじめじめした所でニャーニャー泣いていた事だけは記憶している。'),
('吾輩はここで始めて人間というものを見た。'),
('しかもあとで聞くとそれは書生という人間中で一番獰悪どうあくな種族であったそうだ。'),
('この書生というのは時々我々を捕つかまえて煮にて食うという話である。'),
('しかしその当時は何という考もなかったから別段恐しいとも思わなかった。'),
('ただ彼の掌てのひらに載せられてスーと持ち上げられた時何だかフワフワした感じがあったばかりである。'),
('掌の上で少し落ちついて書生の顔を見たのがいわゆる人間というものの見始みはじめであろう。'),
('この時妙なものだと思った感じが今でも残っている。'),
('第一毛をもって装飾されべきはずの顔がつるつるしてまるで薬缶やかんだ。'),
('その後ご猫にもだいぶ逢あったがこんな片輪かたわには一度も出会でくわした事がない。'),
('のみならず顔の真中があまりに突起している。'),
('そうしてその穴の中から時々ぷうぷうと煙けむりを吹く。'),
('どうも咽むせぽくて実に弱った。'),
('これが人間の飲む煙草たばこというものである事はようやくこの頃知った。');

UPDATE documents SET embedding = tokenize(passage, 'lindera_ipadic');

CREATE INDEX documents_embedding_bm25 ON documents USING bm25 (embedding bm25_ops);

SELECT id, passage, embedding <&> to_bm25query('documents_embedding_bm25', tokenize('書生', 'lindera_ipadic')) AS rank
FROM documents
ORDER BY rank
LIMIT 10;
```

</details>

---

## Tokenizer

Tokenizer selection determines both recall quality and indexing speed. Full details are available in the [Tokenizer Documentation](https://vchord.pgsty.com/bm25/tokenizer/overview/).

### Tokenizer Options

- **[Pre-Trained Models](https://vchord.pgsty.com/bm25/tokenizer/models/)**: Ready out of the box with minimal setup (`bert_base_uncased`, `wiki_tocken`, `gemma2b`, `llmlingua2`). Best when your corpus matches general-purpose language distributions.
- **[Custom Models](https://vchord.pgsty.com/bm25/tokenizer/text-analyzer/)**: Trained directly on your table columns via customizable character filters, pre-tokenizers, and token filters. Ideal for domain-specific terminology, abbreviations, and non-English corpora.

### Key Considerations

1. **Language and Script**:
   - **Space-Separated Languages** (e.g., English, Spanish, German): Standard `unicode_segmentation` or `bert_base_uncased` works well.
   - **Non-Space-Separated Languages** (e.g., Chinese, Japanese): Use language-aware pre-tokenizers such as [Chinese `jieba`](#using-jieba-for-chinese-text) or [Japanese `lindera`](#using-lindera-for-japanese-text), or see the [Multilingual Guide](https://vchord.pgsty.com/bm25/tokenizer/multilingual/).
   - **Multilingual Corpora**: Pre-trained models like `gemma2b` or `llmlingua2` handle mixed scripts in a single index.
2. **Vocabulary Complexity**:
   - **General Text**: Pre-trained models handle everyday vocabulary without training overhead.
   - **Domain-Specific Text**: Technical terms, identifiers, or abbreviations (e.g., `k8s`) benefit from [custom models](#using-a-custom-model) and synonym/stopword dictionaries.

### Model Preloading

By default, PostgreSQL loads a tokenizer model on first use in each backend connection. To avoid first-query latency, preload models at server startup:

```bash
psql -c "SELECT add_preload_model('model1')"
sudo systemctl restart postgresql
```

> [!NOTE]
> Large pre-trained models consume memory when preloaded (~100 MB for `gemma2b`, ~200 MB for `llmlingua2`). Manage preloaded models via `add_preload_model` and `remove_preload_model`.

---

## Comparison with Other PostgreSQL Solutions

- **Built-in `tsvector` + `GIN`**: PostgreSQL's native full-text search lacks BM25 corpus-level inverse document frequency (IDF) and length normalization, and evaluates `ts_rank` across all matching rows before sorting.
- **ParadeDB (`pg_search`)**: Embeds Tantivy as a full-featured search engine replacement inside PostgreSQL with its own query DSL.
- **`vchord_bm25`**: Focuses purely on native BM25 ranking inside PostgreSQL via a lightweight Block-WeakAnd index access method and standard SQL operators (`<&>`), designed to compose naturally with `pgvector` and `vchord`.

## Limitations

- Index scans return up to `bm25_catalog.bm25_limit` candidates (default `100`). Increase `bm25_catalog.bm25_limit` when requesting larger `LIMIT` clauses or combining with selective `WHERE` filters.

---

## Reference

See the full [SQL Reference](https://vchord.pgsty.com/bm25/reference/sql/) and [GUC Reference](https://vchord.pgsty.com/bm25/reference/gucs/) for complete documentation.

### Data Types

- `bm25vector`: Sparse vector storing token IDs and term frequencies (e.g., `{1:2, 2:1}` means token ID `1` appears twice and token ID `2` appears once).
- `bm25query`: Composite query value pairing a target `bm25` index OID with a query `bm25vector`.

### Functions & Operators

- `to_bm25query(regclass, bm25vector) RETURNS bm25query`: Construct a BM25 query against a specific `bm25` index.
- `bm25vector <&> bm25query RETURNS float4`: Compute the **negative** BM25 score (more negative = more relevant, enabling standard ascending `ORDER BY`).
- `bm25vector = bm25vector RETURNS boolean` / `bm25vector <> bm25vector RETURNS boolean`: Equality and inequality comparison.
- `int[]::bm25vector (implicit)`: Cast an integer array of token IDs into a frequency-aggregated `bm25vector` (e.g., `{1, 2, 1}` → `{1:2, 2:1}`).

### GUC Parameters

- `bm25_catalog.bm25_limit` (`integer`, default `100`, range `-1`..`65535`): Maximum candidates returned by a `bm25` index scan (`-1` forces a brute-force scan).
- `bm25_catalog.enable_index` (`boolean`, default `true`): Enable or disable the `bm25` index planner path.
- `bm25_catalog.segment_growing_max_page_size` (`integer`, default `4096`, range `1`..`1000000`): Maximum page count of a growing segment before sealing.

---

## Documentation

Full documentation is available at **[vchord.pgsty.com/bm25](https://vchord.pgsty.com/bm25/)**:

- **Getting Started**: [Overview](https://vchord.pgsty.com/bm25/getting-started/overview/) · [Quick Start](https://vchord.pgsty.com/bm25/getting-started/quick-start/) · [Installation](https://vchord.pgsty.com/bm25/getting-started/installation/)
- **Usage**: [Indexing](https://vchord.pgsty.com/bm25/usage/indexing/) · [Querying](https://vchord.pgsty.com/bm25/usage/querying/) · [Filtering](https://vchord.pgsty.com/bm25/usage/filtering/) · [Hybrid Search](https://vchord.pgsty.com/bm25/usage/hybrid-search/) · [Algorithms](https://vchord.pgsty.com/bm25/usage/algorithms/)
- **Tokenizer**: [Overview](https://vchord.pgsty.com/bm25/tokenizer/overview/) · [Models](https://vchord.pgsty.com/bm25/tokenizer/models/) · [Text Analyzer](https://vchord.pgsty.com/bm25/tokenizer/text-analyzer/) · [Multilingual](https://vchord.pgsty.com/bm25/tokenizer/multilingual/)
- **Reference**: [SQL Reference](https://vchord.pgsty.com/bm25/reference/sql/) · [GUC Reference](https://vchord.pgsty.com/bm25/reference/gucs/) · [Benchmarks](https://vchord.pgsty.com/bm25/reference/benchmarks/) · [Versions](https://vchord.pgsty.com/bm25/reference/versions/)

---

## License

This software is licensed under the **GNU Affero General Public License v3.0 (AGPLv3)**. You may use, modify, and distribute this software under the terms of the [AGPLv3](LICENSE).

- Copyright (c) 2025-2026 TensorChord Inc.
- Copyright (c) 2026 PGSTY PTE. LTD.
- Maintained by [Ruohang Feng](mailto:rh@vonng.com) @ [PGSTY](https://pgsty.com)
