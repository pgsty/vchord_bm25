# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU Affero General Public License v3 (AGPLv3).
# See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
#
# Copyright (c) 2025-2026 TensorChord Inc.
# Copyright (c) 2026 PGSTY PTE. LTD.
# Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

"""PG18+ acceptance in disposable clusters; never connects to an existing server.

Requires psycopg[binary], a PostgreSQL development installation with pageinspect,
and `PG_CONFIG=... make build`. Run with the same pg_config and Python environment:
  python tests/acceptance.py --pg-config /path/to/pg_config [--slt] [--recovery]
The recovery option deliberately kills only backends of its own temporary cluster.
"""
import argparse
import bisect
import collections
import contextlib
import json
import math
import os
from pathlib import Path
import random
import shutil
import signal
import statistics
import struct
import subprocess
import tempfile
import threading
import time

import psycopg

REPO = Path(__file__).resolve().parents[1]


class Cluster:
    def __init__(self, pg_config):
        self.bin = Path(subprocess.check_output([pg_config, '--bindir'], text=True).strip())
        self.tmp = tempfile.TemporaryDirectory(prefix='bm25-acceptance-',dir='/tmp')
        self.root = Path(self.tmp.name)
        self.data = self.root / 'data'
        self.port = 58943  # Unix socket directory makes this private to this run.
        self.dsn = f'host={self.root} port={self.port} user=bm25test dbname=postgres'
        self.standby = None

    def command(self, *args):
        return subprocess.run([str(self.bin / args[0]), *map(str, args[1:])],
                              check=True, capture_output=True, text=True)

    def __enter__(self):
        self.command('initdb', '-D', self.data, '-U', 'bm25test', '-A', 'trust', '--no-locale', '-E', 'UTF8')
        shutil.copytree(REPO / 'build/sharedir', self.root / 'share')
        library = next((REPO / 'build/pkglibdir').glob('vchord_bm25.*'))
        shutil.copy2(library, self.root / library.name)
        control = self.root / 'share/extension/vchord_bm25.control'
        control.write_text(control.read_text().replace("module_pathname = 'vchord_bm25'", f"module_pathname = '{self.root}/vchord_bm25'"))
        with (self.data / 'postgresql.conf').open('a') as f:
            f.write(f"\nlisten_addresses=''\nport={self.port}\nunix_socket_directories='{self.root}'\n"
                    f"shared_preload_libraries='{self.root}/vchord_bm25'\n"
                    f"dynamic_library_path='{self.root}:$libdir'\n"
                    f"extension_control_path='{self.root}/share:$system'\n"
                    "autovacuum=off\nmax_parallel_workers_per_gather=0\n"
                    "wal_level=replica\nmax_wal_senders=5\nmax_replication_slots=5\n"
                    "log_min_messages=debug1\n")
        self.command('pg_ctl', '-D', self.data, '-l', self.root / 'server.log', '-w', '-t', 15, 'start')
        try:
            with self.connect() as c:
                c.execute('CREATE EXTENSION vchord_bm25; CREATE EXTENSION pageinspect')
        except BaseException as error:
            self.__exit__(type(error), error, None)
            raise
        return self

    def connect(self):
        c = psycopg.connect(self.dsn, autocommit=True)
        c.execute("SET statement_timeout='30s'")
        return c

    def __exit__(self, kind, value, tb):
        if kind:
            print((self.root / 'server.log').read_text()[-16000:])
        for data in [self.standby, self.data]:
            if data and (data / 'postmaster.pid').exists():
                self.command('pg_ctl', '-D', data, '-m', 'immediate' if kind else 'fast', '-w', 'stop')
        self.tmp.cleanup()


def scalar(c, sql, args=None):
    return c.execute(sql, args).fetchone()[0]


def state(c, index):
    # Explicit format-4 inspector, test-only. Native endianness matches this server.
    pages = [bytes(row[0]) for row in c.execute(
        "SELECT get_raw_page(%s, n) FROM generate_series(0,pg_relation_size(%s::regclass)::int/8192-1)n",
        (index, index))]
    def item(page):
        off = struct.unpack_from('=I', page, 24)[0] & 0x7fff
        return page[off:]
    meta = item(pages[0])
    assert struct.unpack_from('=Q', meta, 8)[0] == 4, 'update test inspector for new format'
    jump = item(pages[struct.unpack_from('=I', meta, 32)[0]])
    generation, horizon = struct.unpack_from('=QQ', jump, 64)
    active = orphan = free = 0
    for page in pages[1:]:
        upper, = struct.unpack_from('=H', page, 14)
        if not upper:
            free += 1
            continue
        _, flags, gen = struct.unpack_from('=IIQ', page, 8192-16)
        if flags & 1:
            free += 1
        else:
            active += 1
            orphan += gen > generation
    return dict(generation=generation, horizon=horizon, active=active, orphan=orphan,
                free=free, pages=len(pages), sealed=struct.unpack_from('=I', jump, 4)[0])


def score_formula(records, terms):
    # Independent BM25 oracle. Construct the documented 8-bit length buckets
    # algebraically, not by calling extension scoring or reading its table.
    buckets = list(range(40)) + [24 + (8 + (i & 7)) * (1 << ((i >> 3) - 1)) for i in range(16, 232)]
    docs = [d for d in records.values() if d is not None]
    avg = sum(sum(d.values()) for d in docs) / max(1, len(docs)) or 1.0
    df = {t: sum(t in d for d in docs) for t in terms}
    result = {}
    for ident, doc in records.items():
        if doc is None:
            result[ident] = None
            continue
        length = buckets[bisect.bisect_right(buckets, sum(doc.values()))-1]
        result[ident] = -sum(math.log((len(docs)+1)/(df[t]+.5)) *
            (doc[t]*2.2)/(doc[t]+1.2*(.25+.75*length/avg)) for t in terms if t in doc)
    return result


def insert(c, records):
    # Explicit tsvectors avoid stemming and preserve exact frequencies for the oracle.
    def vector(doc):
        if doc is None:
            return None
        return ' '.join("'%s':%s" % (term, ','.join(map(str, range(1, n+1)))) for term, n in sorted(doc.items()))
    with c.cursor().copy('COPY oracle (id, v) FROM STDIN') as copy:
        for ident, doc in records.items():
            copy.write_row((ident, vector(doc)))


def differential(c, physical, visible, terms, *, full=True):
    query = ' '.join(terms)
    sql = "SELECT id,v <&> to_bm25query(to_tsvector('simple',%s),'oracle_bm') s FROM oracle ORDER BY s"
    c.execute('SET enable_seqscan=off; SET enable_sort=off; SET bm25.enable_scan=on')
    plan = str(c.execute('EXPLAIN (FORMAT JSON) '+sql+' LIMIT 17', (query,)).fetchone())
    assert 'oracle_bm' in plan and 'Index Scan' in plan, plan
    actual = c.execute(sql + ('' if full else ' LIMIT 17'), (query,)).fetchall()
    expected = score_formula(physical, terms)
    assert len({i for i, _ in actual}) == len(actual), 'duplicate TID result'
    if full:
        assert {i for i, _ in actual} == set(visible), 'missing or spurious rows'
    for ident, score in actual:
        ref = expected[ident]
        assert (score is None) == (ref is None), (ident, score, ref)
        if score is not None:
            assert math.isclose(score, ref, rel_tol=2e-13, abs_tol=1e-13), (ident, score, ref)
    order = [float('inf') if score is None else score for _, score in actual]
    assert order == sorted(order), 'scan order disagrees with its displayed scores'
    c.execute('SET bm25.enable_scan=off; RESET enable_seqscan; RESET enable_sort')
    brute = c.execute(sql + ('' if full else ' LIMIT 17'), (query,)).fetchall()
    assert [s for _, s in actual] == [s for _, s in brute], 'index and scalar order differ'
    # Tied IDs can differ, but the multiset of full results cannot.
    if full:
        assert sorted(actual) == sorted(brute)
    c.execute('RESET bm25.enable_scan')


def correctness(cluster):
    rng = random.Random(20261103)
    with cluster.connect() as c:
        c.execute('CREATE TABLE oracle (id int PRIMARY KEY, v tsvector)')
        physical = {i: collections.Counter(rng.choices([f'w{j}' for j in range(32)], k=rng.randrange(1,200))) for i in range(1,1501)}
        physical.update({1501: {}, 1502: None})
        insert(c, physical)
        c.execute('CREATE INDEX oracle_bm ON oracle USING bm25(v bm25_ops) WITH(limit=3,maintenance_min_rows=100000,maintenance_scale_factor=1)')
        visible = dict(physical)
        differential(c, physical, visible, ['w1','w3','w5'])
        # Both directions of avgdl drift; candidate batch boundaries and ties.
        for round_no in range(3):
            added = {10000+round_no*500+i: collections.Counter(rng.choices(['w1','w3','w5','w31'],k=(220 if round_no%2 else 2))) for i in range(500)}
            insert(c, added); physical.update(added); visible.update(added)
            for prefilter in ('on','off'):
                c.execute('SET bm25.prefilter='+prefilter)
                differential(c, physical, visible, ['w1','w3','w5'])
            deleted = [i for i in visible if i % 7 == round_no]
            c.execute('DELETE FROM oracle WHERE id = ANY(%s)', (deleted,))
            for i in deleted: del visible[i]
            c.execute('VACUUM (INDEX_CLEANUP ON) oracle')
            assert state(c,'oracle_bm')['generation'] == 0, 'threshold did not defer merge'
            differential(c, physical, visible, ['w1','w3','w5'])
        for terms in ([], ['absent'], ['w31']):
            differential(c, physical, visible, terms)
        # TID reuse after bulkdelete, while old sealed tombstones remain.
        added={20000+i: collections.Counter({'w1':1}) for i in range(500)}
        insert(c,added);physical.update(added);visible.update(added)
        differential(c,physical,visible,['w1'])
        # OFFSET and post-filtering must keep requesting candidates.
        c.execute('SET enable_seqscan=off; SET enable_sort=off; SET bm25.prefilter=off')
        sql="SELECT id FROM oracle WHERE id%11=0 ORDER BY v <&> to_bm25query('w1:1'::tsvector,'oracle_bm'),id OFFSET 13 LIMIT 70"
        got=c.execute(sql).fetchall()
        c.execute('RESET enable_seqscan; RESET enable_sort; SET bm25.enable_scan=off')
        assert got == c.execute(sql).fetchall()
        c.execute('RESET bm25.enable_scan; SET bm25.prefilter=on; SET enable_seqscan=off; SET enable_sort=off; CREATE SEQUENCE filter_calls')
        c.execute("SELECT id FROM oracle WHERE nextval('filter_calls')>0 ORDER BY v <&> to_bm25query('w1:1'::tsvector,'oracle_bm') LIMIT 10").fetchall()
        assert scalar(c,'SELECT last_value FROM filter_calls')==10, 'volatile predicate was evaluated speculatively'
        c.execute('DROP SEQUENCE filter_calls; RESET bm25.prefilter; RESET enable_seqscan; RESET enable_sort')
        # A stable query freezes statistics only for that execution, including prepared SQL.
        c.execute("PREPARE score AS SELECT v <&> to_bm25query('w1:1'::tsvector,'oracle_bm') FROM oracle WHERE id=20000")
        before=scalar(c,'EXECUTE score')
        c.execute("SET plan_cache_mode=force_generic_plan; PREPARE generic(bm25query) AS SELECT count(*) FROM(SELECT id FROM oracle ORDER BY v <&> $1 LIMIT 70)s")
        assert scalar(c,"EXECUTE generic(to_bm25query('w1:1'::tsvector,'oracle_bm'))")==70
        c.execute('RESET plan_cache_mode')
        c.execute("INSERT INTO oracle VALUES(30000,'w31:1')")
        physical[30000]=visible[30000]=collections.Counter({'w31':1})
        assert before != scalar(c,'EXECUTE score')
        assert scalar(c,"SELECT bm25_maintain('oracle_bm')")
        physical=dict(visible)
        differential(c,physical,visible,['w1','w3','w5'])
        c.execute('VACUUM (ANALYZE) oracle')
        assert abs(scalar(c,"SELECT reltuples FROM pg_class WHERE oid='oracle_bm'::regclass")-len(visible))<1
        before=state(c,'oracle_bm')
        c.execute('VACUUM (ANALYZE) oracle')
        assert state(c,'oracle_bm')['generation']==before['generation'], 'unchanged corpus was rewritten'
        c.execute('DROP TABLE oracle')
    print('PASS independent formula, WAND drift, tombstones/TID reuse, complete scans, prepared execution',flush=True)


def lifecycle(cluster):
    with cluster.connect() as c, cluster.connect() as reader:
        c.execute("CREATE TABLE life(id int PRIMARY KEY, v tsvector); INSERT INTO life SELECT i,to_tsvector('simple','term '||i) FROM generate_series(1,5000)i; CREATE INDEX life_bm ON life USING bm25(v bm25_ops) WITH(maintenance_min_rows=1,maintenance_scale_factor=0)")
        reader.execute('BEGIN ISOLATION LEVEL REPEATABLE READ');reader.execute('SELECT * FROM life WHERE id=1')
        c.execute("INSERT INTO life VALUES(5001,'term:1')")
        c.execute('VACUUM life')
        assert state(c,'life_bm')['generation']==1, 'busy reader prevented publication'
        c.execute("INSERT INTO life VALUES(5002,'term:1')");c.execute('VACUUM life')
        assert state(c,'life_bm')['generation']==1, 'old snapshot did not protect retired pages'
        reader.execute('SET enable_seqscan=off; SET enable_sort=off')
        assert scalar(reader,"SELECT count(*) FROM(SELECT id FROM life ORDER BY v <&> to_bm25query('term:1'::tsvector,'life_bm'))s")==5000
        reader.execute('ROLLBACK')
        c.execute('VACUUM life');assert state(c,'life_bm')['generation']==2
        # ANALYZE inside a held transaction neither merges nor blocks point lookups.
        reader.execute('BEGIN; ANALYZE life')
        c.execute("SET lock_timeout='100ms'");assert scalar(c,'SELECT id FROM life WHERE id=1')==1
        assert scalar(c,"SELECT count(*) FROM pg_locks WHERE pid=%s AND mode='AccessExclusiveLock'",(reader.info.backend_pid,))==0
        reader.execute('ROLLBACK')
        # Actual parallel worker, not merely VACUUM(PARALLEL n) syntax.
        c.execute('CREATE INDEX life_mod ON life((id%10)); SET min_parallel_index_scan_size=0; DELETE FROM life WHERE id%9=0')
        c.execute('VACUUM (PARALLEL 2, INDEX_CLEANUP ON) life')
        assert 'starting parallel vacuum worker' in (cluster.root/'server.log').read_text(), 'no parallel worker was observed'
        c.execute('CREATE ROLE bm25_maint_user; SET ROLE bm25_maint_user')
        try:
            c.execute("SELECT bm25_maintain('life_bm')")
            raise AssertionError('unprivileged maintenance was allowed')
        except psycopg.errors.InsufficientPrivilege: pass
        c.execute('RESET ROLE; GRANT MAINTAIN ON life TO bm25_maint_user; SET ROLE bm25_maint_user')
        c.execute("SELECT bm25_maintain('life_bm')")
        c.execute('RESET ROLE; REVOKE MAINTAIN ON life FROM bm25_maint_user; DROP ROLE bm25_maint_user')
        c.execute('DROP TABLE life')
    print('PASS busy maintenance, long snapshot, ANALYZE locks, parallel VACUUM',flush=True)


def slt(cluster):
    with cluster.connect() as admin:
        for file in sorted((REPO/'tests/sqllogictest').glob('*.slt')):
            name='slt_'+file.stem
            admin.execute(f'CREATE DATABASE {name}')
            with psycopg.connect(cluster.dsn.replace('dbname=postgres','dbname='+name),autocommit=True) as c:
                c.execute('CREATE EXTENSION vchord_bm25')
            r=subprocess.run(['sqllogictest','-h',str(cluster.root),'-p',str(cluster.port),'-d',name,'-u','bm25test',str(file)],capture_output=True,text=True)
            assert r.returncode==0,r.stdout+r.stderr
            admin.execute(f'DROP DATABASE {name} WITH (FORCE)')
            print('PASS SLT',file.stem,flush=True)


def wait_for(fn, description, timeout=45):
    deadline=time.monotonic()+timeout
    while time.monotonic()<deadline:
        result=fn()
        if result: return result
        time.sleep(.01)
    raise AssertionError('timed out: '+description)


def explain(c, sql):
    return scalar(c,'EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) '+sql)[0]


def performance(cluster):
    with cluster.connect() as c:
        c.execute("CREATE TABLE perf(id int PRIMARY KEY,v tsvector); INSERT INTO perf SELECT i,to_tsvector('simple',CASE WHEN i%2=0 THEN 'common ' ELSE '' END || repeat('word ',i%70+1)||'w'||i%499) FROM generate_series(1,80000)i; CREATE INDEX perf_bm ON perf USING bm25(v bm25_ops); ANALYZE perf")
        c.execute('SET enable_seqscan=off; SET enable_sort=off')
        sql="SELECT id FROM perf ORDER BY v <&> to_bm25query(to_tsvector('simple','common w1 w2'),'perf_bm') LIMIT 10"
        before=[explain(c,sql) for _ in range(5)]
        c.execute("INSERT INTO perf VALUES(80001,to_tsvector('simple','common word w1'))")
        after=[explain(c,sql) for _ in range(5)]
        hits=lambda samples:statistics.median(x['Plan']['Shared Hit Blocks'] for x in samples)
        # Buffer work catches disabled WAND without a wall-clock microbenchmark.
        assert hits(after) <= max(hits(before)*4,hits(before)+100), (hits(before),hits(after))
        c.execute('RESET enable_seqscan; RESET enable_sort; SET bm25.enable_scan=off')
        timings=[]
        for round_no in range(3):
            start=100000+round_no*50000
            c.execute(f"INSERT INTO perf SELECT i,to_tsvector('simple','common word w1') FROM generate_series({start},{start+19999})i")
            samples=[]
            for j in range(5):
                lo=start+20000+j*1000
                t=time.perf_counter();c.execute(f"INSERT INTO perf SELECT i,to_tsvector('simple','common word') FROM generate_series({lo},{lo+999})i")
                samples.append(time.perf_counter()-t)
            timings.append(statistics.median(samples))
        assert max(timings) < 4*min(timings)+.02,timings
        prefix="SELECT v <&> to_bm25query('common:1'::tsvector,'perf_bm') FROM perf ORDER BY id LIMIT "
        one=explain(c,prefix+'1')['Plan']['Shared Hit Blocks']
        hundred=explain(c,prefix+'100')['Plan']['Shared Hit Blocks']
        assert hundred < one*2+1000,(one,hundred)
        # Growing collection is interruptible even in a scalar-only plan.
        c.execute("SET statement_timeout='1ms'")
        t=time.perf_counter()
        try:
            c.execute(prefix+'100').fetchall()
            raise AssertionError('scalar preparation finished before timeout fixture fired')
        except psycopg.errors.QueryCanceled:
            assert time.perf_counter()-t<.5
        c.execute("SET statement_timeout='30s'; RESET bm25.enable_scan")
        c.execute('REINDEX INDEX CONCURRENTLY perf_bm')
        c.execute('DROP TABLE perf')
    print('PASS WAND buffer work',hits(before),hits(after),'insert median seconds',timings,'scalar buffers',one,hundred,flush=True)


def background(cluster, sql, slow=False):
    c=cluster.connect()
    if slow: c.execute("SET statement_timeout='90s'; SET vacuum_cost_delay='2ms'; SET vacuum_cost_limit=10")
    errors=[]
    def run():
        try: c.execute(sql)
        except psycopg.Error as error: errors.append(error)
    worker=threading.Thread(target=run)
    worker.start()
    return c,worker,errors


def recovery(cluster):
    c=cluster.connect()
    c.execute("CREATE TABLE rec(id int PRIMARY KEY,v tsvector); INSERT INTO rec SELECT i,to_tsvector('simple','term '||repeat('word ',i%80+1)||i) FROM generate_series(1,60000)i; CREATE INDEX rec_bm ON rec USING bm25(v bm25_ops) WITH(maintenance_min_rows=1,maintenance_scale_factor=0)")
    c.execute("INSERT INTO rec SELECT i,to_tsvector('simple','term '||i) FROM generate_series(60001,65000)i")
    original=state(c,'rec_bm')
    # Cancel after observing actual unpublished output, not during setup.
    conn,worker,errors=background(cluster,'VACUUM rec',slow=True)
    wait_for(lambda:state(c,'rec_bm')['orphan']>32,'unpublished rebuild pages')
    c.execute("INSERT INTO rec VALUES(65004,'term:1')")
    conn.cancel();worker.join(5);assert not worker.is_alive()
    assert errors and errors[0].sqlstate=='57014',errors
    conn.close()
    assert state(c,'rec_bm')['orphan']>0
    c.execute('VACUUM rec')
    assert state(c,'rec_bm')['orphan']==0
    original=state(c,'rec_bm')
    c.execute("INSERT INTO rec VALUES(65005,'term:1')")
    # A second rebuild crashes after it has allocated output. Only our test PID.
    conn,worker,errors=background(cluster,'VACUUM rec',slow=True)
    wait_for(lambda:state(c,'rec_bm')['orphan']>32,'unpublished crash pages')
    os.kill(conn.info.backend_pid,signal.SIGKILL)
    worker.join(5);conn.close();c.close()
    def reconnect():
        try: return cluster.connect()
        except psycopg.Error: return None
    c=wait_for(reconnect,'postmaster crash recovery')
    assert state(c,'rec_bm')['generation']==original['generation']
    for _ in range(2):
        c.execute('SELECT pg_current_xact_id()');c.execute('VACUUM rec')
    assert state(c,'rec_bm')['orphan']==0
    # Publish a second generation; kill recycling after at least one batch.
    c.execute("INSERT INTO rec VALUES(65001,'term:1')");c.execute('VACUUM rec')
    c.execute('SELECT pg_current_xact_id()')
    before=state(c,'rec_bm');assert before['horizon']!=0
    conn,worker,errors=background(cluster,'VACUUM rec',slow=True)
    wait_for(lambda:(lambda s:s['free']>before['free'] and s['horizon']!=0)(state(c,'rec_bm')),'partially retired generation')
    os.kill(conn.info.backend_pid,signal.SIGKILL)
    worker.join(5);conn.close();c.close()
    c=wait_for(reconnect,'recycle crash recovery')
    for _ in range(2):c.execute('SELECT pg_current_xact_id()');c.execute('VACUUM rec')
    cleaned=state(c,'rec_bm');assert cleaned['orphan']==0 and cleaned['horizon']==0,cleaned
    c.execute('SET enable_seqscan=off; SET enable_sort=off')
    assert c.execute("SELECT count(*),count(DISTINCT id) FROM(SELECT id FROM rec ORDER BY v <&> to_bm25query('term:1'::tsvector,'rec_bm'))s").fetchone()==(65003,65003)
    c.execute('RESET enable_seqscan; RESET enable_sort')
    pages=cleaned['pages']
    for _ in range(3):c.execute('VACUUM rec')
    assert state(c,'rec_bm')['pages']==pages,'FSM recovery failed to stabilize allocation'
    c.execute('REINDEX INDEX rec_bm')
    compact=state(c,'rec_bm')
    assert cleaned['active']<=compact['active']*1.05+2,(cleaned,compact)
    print('PASS cancel/crash during build and retirement, FSM repair',cleaned,'reindexed',compact,flush=True)
    standby(cluster,c)
    c.execute('DROP TABLE rec');c.close()


def standby(cluster, primary):
    cluster.standby=cluster.root/'standby'
    cluster.command('pg_basebackup','-D',cluster.standby,'-h',cluster.root,'-p',cluster.port,'-U','bm25test','-X','stream','-R','--checkpoint=fast')
    with (cluster.standby/'postgresql.conf').open('a') as f:
        f.write("\nport=58944\nhot_standby_feedback=off\nmax_standby_streaming_delay=-1\n")
    cluster.command('pg_ctl','-D',cluster.standby,'-l',cluster.root/'standby.log','-w','-t',15,'start')
    dsn=cluster.dsn.replace('port=58943','port=58944')
    observer=psycopg.connect(dsn,autocommit=True)
    def sync():
        lsn=scalar(primary,'SELECT pg_current_wal_insert_lsn()')
        wait_for(lambda:scalar(observer,'SELECT pg_last_wal_replay_lsn()>=%s::pg_lsn',(lsn,)),'standby WAL replay')
    sync()
    def cursor():
        reader=psycopg.connect(dsn)
        reader.execute('SET enable_seqscan=off; SET enable_sort=off; SET "bm25.limit"=3')
        cur=reader.cursor(name='bm25_reader')
        cur.execute("SELECT id FROM rec ORDER BY v <&> to_bm25query('term:1'::tsvector,'rec_bm')")
        assert len(cur.fetchmany(1))==1
        return reader,cur
    # No heap deletion: the extension itself must reject recycled addresses.
    reader,cur=cursor()
    primary.execute("INSERT INTO rec VALUES(65002,'term:1'); SELECT bm25_maintain('rec_bm')")
    primary.execute("INSERT INTO rec VALUES(65003,'term:1'); SELECT bm25_maintain('rec_bm')")
    primary.execute('SELECT pg_current_xact_id()')
    assert scalar(primary,"SELECT bm25_maintain('rec_bm')")
    sync()
    assert state(observer,'rec_bm')['generation']==2
    try:
        cur.fetchall();raise AssertionError('standby accepted a recycled generation')
    except psycopg.errors.SerializationFailure as error:
        assert 'bm25' in str(error),error
    reader.rollback();reader.close()
    sync()
    # Tombstones need no extension-level conflict: VACUUM marks only TIDs whose
    # heap tuples it already pruned, and replaying that prune record resolves
    # conflicts with standby snapshots that could still see them. A standby
    # snapshot taken after the DELETE must keep working across the replay.
    primary.execute('ALTER INDEX rec_bm SET(maintenance_min_rows=1000000,maintenance_scale_factor=1)')
    primary.execute('DELETE FROM rec WHERE id%5=0')
    # Publish running-xact state too: read-only XID assignments need not emit
    # commit WAL, and can otherwise keep a standby's xmin artificially old.
    primary.execute('CHECKPOINT')
    sync();generation=state(observer,'rec_bm')['generation']
    reader=psycopg.connect(dsn)
    reader.execute('BEGIN ISOLATION LEVEL REPEATABLE READ')
    reader.execute('SET enable_seqscan=off; SET enable_sort=off')
    total=reader.execute('SELECT count(*) FROM rec').fetchone()[0]
    primary.execute('SELECT pg_current_xact_id()')
    primary.execute('VACUUM rec')
    sync()
    assert state(observer,'rec_bm')['generation']==generation
    ids=[r[0] for r in reader.execute("SELECT id FROM rec ORDER BY v <&> to_bm25query('term:1'::tsvector,'rec_bm')").fetchall()]
    assert len(ids)==total==len(set(ids)) and not any(i%5==0 for i in ids),(len(ids),total)
    reader.rollback();reader.close();sync()
    observer.execute('SET enable_seqscan=off; SET enable_sort=off')
    assert scalar(observer,"SELECT count(*) FROM(SELECT id FROM rec ORDER BY v <&> to_bm25query('term:1'::tsvector,'rec_bm'))s")==scalar(primary,'SELECT count(*) FROM rec')
    observer.close()
    print('PASS physical standby recycling (40001) and tombstone replay without false conflicts',flush=True)


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--pg-config',default=os.environ.get('PG_CONFIG','pg_config'))
    parser.add_argument('--slt',action='store_true')
    parser.add_argument('--recovery',action='store_true')
    args=parser.parse_args()
    with Cluster(args.pg_config) as cluster:
        correctness(cluster)
        lifecycle(cluster)
        performance(cluster)
        if args.slt: slt(cluster)
        if args.recovery: recovery(cluster)


if __name__=='__main__': main()
