# frozen_string_literal: true

require "spec_helper"

RSpec.describe "Gremlin local file CLI integration" do
  around do |example|
    Dir.mktmpdir("gremlin-rspec-") do |dir|
      @workspace = dir
      @db_path = File.join(dir, "gremlin-test.db")
      @fixture_root = File.join(dir, "fixture-root")
      @dest_root = File.join(dir, "dest-root")
      FileUtils.mkdir_p(@fixture_root)
      FileUtils.mkdir_p(@dest_root)
      reset_db!
      example.run
    end
  end

  attr_reader :db_path, :fixture_root, :dest_root

  let(:base_time) { Time.utc(2026, 7, 8, 12, 0, 0) }

  def build_fixture_tree
    write_fixture("root.txt", "root file\n", mtime: base_time)
    write_fixture("alpha/one.txt", "alpha one\n", mtime: base_time + 1)
    write_fixture("alpha/nested/two.bin", "two\x00bin\n", mtime: base_time + 2)
  end

  def transfer_plan_id(stdout)
    stdout[/^transfer_plan:\t(.+)$/, 1] or raise "missing transfer_plan line:\n#{stdout}"
  end

  it "fast imports a directory tree with stat metadata only" do
    build_fixture_tree

    expect(gremlin!("init").stdout).to include("initialized")

    scan = gremlin_json!("scan", fixture_root)
    expect(scan.fetch("files_seen")).to eq(3)
    expect(scan.fetch("new_count")).to eq(3)
    expect(scan.fetch("changed_count")).to eq(0)
    expect(scan.fetch("missing_count")).to eq(0)
    expect(scan.fetch("deltas").map { |delta| delta.fetch("relative_path") }).to contain_exactly(
      "alpha/nested/two.bin",
      "alpha/one.txt",
      "root.txt"
    )

    status = gremlin_json!("status", fixture_root)
    expect(status.fetch("known")).to eq(true)
    expect(status.fetch("files")).to eq(3)
    expect(status.fetch("content_objects")).to eq(0)

    rows = rows_by_path
    expect(rows.keys).to contain_exactly("alpha/nested/two.bin", "alpha/one.txt", "root.txt")
    expect(rows.values.map { |row| row.fetch(:status) }).to all(eq("present"))
    expect(rows.values.map { |row| row.fetch(:content_id) }).to all(eq("-"))
  end

  it "hash imports a directory tree and exposes content ids through the CLI" do
    build_fixture_tree
    gremlin!("init")

    preview = gremlin_json!("hash-preview", fixture_root)
    expect(preview.fetch("candidates")).to eq(3)
    expect(preview.fetch("skipped_unchanged")).to eq(0)
    expect(preview.fetch("candidate_files").map { |row| row.fetch("relative_path") }).to contain_exactly(
      "alpha/nested/two.bin",
      "alpha/one.txt",
      "root.txt"
    )

    hash = gremlin_json!("hash", fixture_root, "--all")
    expect(hash.fetch("files_hashed")).to eq(3)
    expect(hash.fetch("skipped_unchanged")).to eq(0)
    expect(hash.fetch("errors")).to eq(0)
    expect(hash.fetch("hashed_paths")).to contain_exactly(
      "alpha/nested/two.bin",
      "alpha/one.txt",
      "root.txt"
    )

    status = gremlin_json!("status", fixture_root)
    expect(status.fetch("files")).to eq(3)
    expect(status.fetch("content_objects")).to eq(3)
    expect(status.fetch("integrity")).to include(
      "hashed_files" => 3,
      "sha256_files" => 3,
      "crc32_files" => 3,
      "chunk_hashed_files" => 0
    )

    rows = rows_by_path
    expect(rows.values.map { |row| row.fetch(:content_id) }).to all(match(/\Acontent_/))

    unchanged = gremlin_json!("hash", fixture_root)
    expect(unchanged.fetch("files_hashed")).to eq(0)
    expect(unchanged.fetch("skipped_unchanged")).to eq(3)
    expect(unchanged.fetch("errors")).to eq(0)

    clean_preview = gremlin_json!("hash-preview", fixture_root)
    expect(clean_preview.fetch("candidates")).to eq(0)
    expect(clean_preview.fetch("skipped_unchanged")).to eq(3)
  end

  it "detects new, missing, content-changed, and mtime-only metadata changes" do
    build_fixture_tree
    gremlin!("init")
    gremlin_json!("scan", fixture_root)

    write_fixture("alpha/one.txt", "alpha one changed\n", mtime: base_time + 60)
    FileUtils.rm_f(fixture_path("alpha", "nested", "two.bin"))
    write_fixture("alpha/new.txt", "brand new\n", mtime: base_time + 70)
    File.utime(base_time + 80, base_time + 80, fixture_path("root.txt"))

    scan = gremlin_json!("scan", fixture_root)
    expect(scan.fetch("files_seen")).to eq(3)
    expect(scan.fetch("new_count")).to eq(1)
    expect(scan.fetch("changed_count")).to eq(2)
    expect(scan.fetch("missing_count")).to eq(1)

    deltas = scan.fetch("deltas").to_h { |delta| [delta.fetch("relative_path"), delta] }
    expect(deltas.fetch("alpha/new.txt").fetch("kind")).to eq("new")
    expect(deltas.fetch("alpha/one.txt").fetch("kind")).to eq("changed")
    expect(deltas.fetch("root.txt").fetch("kind")).to eq("changed")
    expect(deltas.fetch("alpha/nested/two.bin").fetch("kind")).to eq("missing")

    hash = gremlin_json!("hash", fixture_root)
    expect(hash.fetch("files_hashed")).to eq(3)
    expect(hash.fetch("hashed_paths")).to contain_exactly(
      "alpha/new.txt",
      "alpha/one.txt",
      "root.txt"
    )
  end

  it "accepts a reviewed verify job without re-running verify with --accept" do
    build_fixture_tree
    gremlin!("init")
    gremlin_json!("hash", fixture_root, "--all")

    write_fixture("alpha/one.txt", "alpha one reviewed change\n", mtime: base_time + 120)

    reviewed = gremlin_json!("verify", fixture_root)
    expect(reviewed.fetch("changed")).to eq(1)
    expect(reviewed.fetch("accepted")).to eq(0)

    accepted = gremlin_json!("verify-accept", reviewed.fetch("job_id"))
    expect(accepted.fetch("accepted")).to eq(1)
    expect(accepted.fetch("errors")).to eq(0)

    clean = gremlin_json!("verify", fixture_root)
    expect(clean.fetch("ok")).to eq(3)
    expect(clean.fetch("changed")).to eq(0)
    expect(clean.fetch("errors")).to eq(0)
  end

  it "copies local files through a planned transfer while preserving paths and mtimes" do
    build_fixture_tree
    gremlin!("init")
    gremlin_json!("hash", fixture_root, "--all")
    gremlin!("target", "add", dest_root)

    plan = gremlin!("--details", "transfer", "plan", fixture_root, dest_root, "--all")
    expect(plan.stdout).to include("copy:\t3")
    plan_id = transfer_plan_id(plan.stdout)

    run = gremlin!("transfer", "run", plan_id)
    expect(run.stdout).to include("copied:\t3")
    expect(run.stdout).to include("skipped:\t0")
    expect(run.stdout).to include("errors:\t0")
    expect(run.stdout).to include("canceled:\tfalse")

    expect(File.binread(File.join(dest_root, "root.txt"))).to eq("root file\n")
    expect(File.binread(File.join(dest_root, "alpha", "one.txt"))).to eq("alpha one\n")
    expect(File.binread(File.join(dest_root, "alpha", "nested", "two.bin"))).to eq("two\x00bin\n")
    expect(File.mtime(File.join(dest_root, "root.txt")).to_i).to eq(
      File.mtime(File.join(fixture_root, "root.txt")).to_i
    )

    status = gremlin_json!("status", dest_root)
    expect(status.fetch("known")).to eq(true)
    expect(status.fetch("files")).to eq(3)
    expect(status.fetch("content_objects")).to eq(3)

    verify = gremlin_json!("verify", dest_root)
    expect(verify.fetch("ok")).to eq(3)
    expect(verify.fetch("changed")).to eq(0)
    expect(verify.fetch("missing")).to eq(0)
    expect(verify.fetch("errors")).to eq(0)

    repeat_plan = gremlin!("transfer", "plan", fixture_root, dest_root, "--all")
    expect(repeat_plan.stdout).to include("skip:\t3")
  end

  it "deletes the configured database file only after confirmation" do
    gremlin!("init")
    File.binwrite("#{db_path}-wal", "wal")
    File.binwrite("#{db_path}-shm", "shm")

    preview = gremlin!("db", "delete")
    expect(preview.stdout).to include("confirm:")
    expect(File.exist?(db_path)).to eq(true)
    expect(File.exist?("#{db_path}-wal")).to eq(true)
    expect(File.exist?("#{db_path}-shm")).to eq(true)

    deleted = gremlin!("db", "delete", "--yes")
    expect(deleted.stdout).to include("removed:\t#{db_path}")
    expect(deleted.stdout).to include("deleted:\t3 file(s)")
    expect(File.exist?(db_path)).to eq(false)
    expect(File.exist?("#{db_path}-wal")).to eq(false)
    expect(File.exist?("#{db_path}-shm")).to eq(false)
  end
end
