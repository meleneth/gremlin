# frozen_string_literal: true

require "json"
require "open3"
require "fileutils"
require "tmpdir"

RSpec.configure do |config|
  config.disable_monkey_patching!
  config.expect_with :rspec do |expectations|
    expectations.syntax = :expect
  end

  config.before(:suite) do
    next if ENV["GREMLIN_BIN"]

    success = system("cargo", "build", "--quiet")
    raise "cargo build failed" unless success
  end
end

module GremlinCli
  CommandResult = Struct.new(:stdout, :stderr, :status, keyword_init: true) do
    def success?
      status.success?
    end
  end

  def gremlin_bin
    ENV.fetch("GREMLIN_BIN") { File.expand_path("../target/debug/gremlin", __dir__) }
  end

  def reset_db!
    FileUtils.rm_f(db_path)
  end

  def gremlin(*args)
    stdout, stderr, status = Open3.capture3(
      { "NO_COLOR" => "1" },
      gremlin_bin,
      "--no-config",
      "--db",
      db_path.to_s,
      *args.flatten.map(&:to_s)
    )
    CommandResult.new(stdout: stdout, stderr: stderr, status: status)
  end

  def gremlin!(*args)
    result = gremlin(*args)
    return result if result.success?

    raise <<~ERROR
      gremlin #{args.flatten.join(" ")} failed with #{result.status.exitstatus}
      stdout:
      #{result.stdout}
      stderr:
      #{result.stderr}
    ERROR
  end

  def gremlin_json!(*args)
    JSON.parse(gremlin!("--json", *args).stdout)
  end

  def fixture_path(*parts)
    File.join(fixture_root, *parts)
  end

  def write_fixture(relative_path, contents, mtime:)
    path = fixture_path(*relative_path.split("/"))
    FileUtils.mkdir_p(File.dirname(path))
    File.binwrite(path, contents)
    File.utime(mtime, mtime, path)
  end

  def file_rows
    gremlin!("files").stdout.lines.map do |line|
      size, status, modified_at, content_id, relative_path = line.chomp.split("\t", 5)
      {
        size: Integer(size),
        status: status,
        modified_at: modified_at,
        content_id: content_id,
        relative_path: relative_path
      }
    end
  end

  def rows_by_path
    file_rows.to_h { |row| [row.fetch(:relative_path), row] }
  end
end

RSpec.configure do |config|
  config.include GremlinCli
end
